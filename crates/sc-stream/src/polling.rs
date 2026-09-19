//! [`PollingProvider`]: a provider that is **asked** rather than one that
//! pushes (TODO "Streams" §12, task 9.1).
//!
//! This is the one place a module-supplied provider is shaped differently from
//! a Rust one, and the reason is not taste. A module call is request/response
//! on a Deno worker (`ModuleHost::call`): there is no channel from a worker
//! back into the host, and building one is a milestone of its own. So a module
//! does not subscribe — it declares a `poll`, and **`sc-stream` supplies the
//! loop**:
//!
//! ```js
//! streamproviders: {
//!   poll_feed: {
//!     description: "An RSS feed, polled",
//!     config_fields: [{ name: "url", type: "String", required: true },
//!                     { name: "interval_s", type: "Integer", default: 60 }],
//!     element_type: ({ configuration }) => ({ kind: "json", keys: [ … ] }),
//!     poll: async ({ configuration, cursor }) => ({ elements: [ … ], cursor: "…" }),
//!   },
//! }
//! ```
//!
//! Writing the loop here rather than once per module is the same argument the
//! supervisor's backoff makes (§6): the interval, the cursor, the decoding of
//! every element against the declared type and the behaviour of a poll that
//! throws are the parts every author gets subtly wrong, and there is one of them
//! here.
//!
//! ## A poll that throws ends the subscription
//!
//! It does **not** sleep and try again. The task returns, the [`Subscription`]
//! reports it has ended, and the supervisor restarts it with capped exponential
//! backoff and an attempt count (§6) — which is the machinery that already
//! exists for a broker that hung up. A provider that retried in here would be a
//! second, worse backoff that the Streams screen could not see: the stream would
//! read `running` while every poll failed.
//!
//! ## The cursor is opaque
//!
//! Whatever a poll answers is handed back to the next one, and nothing in this
//! crate looks inside it. It is the module's own "where I got to" — a feed's
//! last item id, a queue's offset — and it lives **in memory only**: a restart
//! starts from `null`, because §OUT puts backfill and replay out of scope and a
//! stored cursor is a durable claim about a flow this system does not store.
//!
//! ## Why the element type is resolved rather than declared
//!
//! GOALS makes the element type a function of the configuration, and for a
//! module that function is JavaScript on a worker — an `async` call across a
//! seam, where [`StreamProvider::element_type`] is a synchronous method that a
//! form handler, the client generator and the Streams list all call. So this
//! provider answers the synchronous one **from what it last resolved**, and
//! [`StreamProvider::resolve_element_type`] is the asynchronous door that fills
//! it: a save resolves before it validates, and the supervisor resolves before
//! it starts. Asking for a configuration nobody has resolved yet is answered
//! with a sentence saying exactly that, not with a guess.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use sc_error::{Error, Result};
use sc_types::{Attrs, FormField};
use serde_json::Value as Json;

use crate::element::{ElementType, RawPayload};
use crate::envelope::Element;
use crate::provider::{StreamProvider, StreamProviderKind, StreamSink};
use crate::subscription::Subscription;

/// The configuration key a polled provider's interval is read from, and the
/// name §12's example declares.
///
/// Read from the provider's *own* configuration rather than being a field of
/// [`PollingProvider`] because it is the admin's decision, on the form, per
/// stream: one feed is polled every five minutes and another every ten seconds.
pub const INTERVAL_FIELD: &str = "interval_s";

/// How often a polled provider that did not say is asked. A minute: often
/// enough for a feed, rare enough that a provider whose author forgot the
/// setting does not hammer somebody else's server.
pub const DEFAULT_INTERVAL_S: u64 = 60;

/// The floor under a configured interval.
///
/// A poll is a call across the module seam and out to somebody else's network,
/// and `interval_s: 0` is not "as fast as possible", it is a loop with no wait
/// in it — the one shape that can starve the worker every other module call
/// shares. A stream that wants elements the moment they exist wants a pushing
/// provider, which is what the MQTT one is.
pub const MIN_INTERVAL: Duration = Duration::from_millis(100);

/// What one poll answered: the elements it found, and where it got to.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PollAnswer {
    /// The elements, as the provider produced them — JSON, because the module
    /// seam is JSON. Each is decoded against the stream's [`ElementType`]
    /// before it is delivered.
    pub elements: Vec<Json>,
    /// The opaque cursor handed to the next poll. `Json::Null` means "start
    /// again from wherever you start from".
    pub cursor: Json,
}

impl PollAnswer {
    /// What a poll answered, read leniently in the one shape a plugin author
    /// will otherwise write by hand.
    ///
    /// `{ elements, cursor }` is the full form. A **bare array** is read as its
    /// elements with no cursor, because a provider that has no cursor — a feed
    /// read whole every time — should not have to wrap its answer to say so.
    pub fn read(answer: Json) -> Result<PollAnswer> {
        match answer {
            Json::Array(elements) => Ok(PollAnswer {
                elements,
                cursor: Json::Null,
            }),
            Json::Object(mut object) => {
                let cursor = object.remove("cursor").unwrap_or(Json::Null);
                let elements = match object.remove("elements") {
                    None | Some(Json::Null) => Vec::new(),
                    Some(Json::Array(elements)) => elements,
                    Some(other) => {
                        return Err(Error::invalid(format!(
                            "a poll's `elements` must be a list, got {other}"
                        )));
                    }
                };
                Ok(PollAnswer { elements, cursor })
            }
            other => Err(Error::invalid(format!(
                "a poll must answer `{{ elements, cursor }}` or a list of elements, got {other}"
            ))),
        }
    }
}

/// Where a [`PollingProvider`]'s two calls actually go — the module seam, in
/// this crate's vocabulary.
///
/// Declared here and implemented in `sc-module::stream_providers` (task 9.2),
/// which routes both to the worker the module is loaded on. A test implements
/// it with a closure over a `Vec`, which is what makes the loop below testable
/// without a Deno worker.
#[async_trait]
pub trait PollHost: Send + Sync {
    /// The element type this provider declares for `config`.
    ///
    /// Asynchronous, unlike [`StreamProvider::element_type`], because the
    /// declaration is a function on the far side of a seam — see the module
    /// docs for what that costs and how it is paid.
    async fn element_type(
        &self,
        provider: &StreamProviderKind,
        config: &Attrs,
    ) -> Result<ElementType>;

    /// One poll. `cursor` is whatever the previous poll answered, or
    /// `Json::Null` for the first.
    async fn poll(
        &self,
        provider: &StreamProviderKind,
        config: &Attrs,
        cursor: &Json,
    ) -> Result<PollAnswer>;
}

/// A [`StreamProvider`] built out of a poll: the interval loop, the cursor and
/// the decoding, supplied by this crate on behalf of code that cannot push.
pub struct PollingProvider {
    kind: StreamProviderKind,
    host: Arc<dyn PollHost>,
    /// The element types resolved so far, by configuration — see the module
    /// docs. Small: one entry per configuration any caller has asked about,
    /// which in practice is one per stream on this provider.
    resolved: Mutex<HashMap<String, ElementType>>,
}

impl PollingProvider {
    /// The provider `kind` declares, polled over `host`.
    pub fn new(kind: StreamProviderKind, host: Arc<dyn PollHost>) -> PollingProvider {
        PollingProvider {
            kind,
            host,
            resolved: Mutex::new(HashMap::new()),
        }
    }

    /// The declaration this was built from — what the picker lists.
    pub fn kind_ref(&self) -> &StreamProviderKind {
        &self.kind
    }

    /// A configuration as a cache key: its keys in sorted order, so two maps
    /// with the same content are one key whatever order they were built in.
    fn key(config: &Attrs) -> String {
        let sorted: BTreeMap<&String, &Json> = config.iter().collect();
        serde_json::to_string(&sorted).unwrap_or_default()
    }

    /// What was last resolved for `config`, if anything.
    fn cached(&self, config: &Attrs) -> Option<ElementType> {
        let guard = match self.resolved.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.get(&Self::key(config)).cloned()
    }

    /// Remember what the module answered for `config`.
    fn remember(&self, config: &Attrs, ty: &ElementType) {
        let mut guard = match self.resolved.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.insert(Self::key(config), ty.clone());
    }

    /// How often this configuration says to poll.
    ///
    /// A missing, unreadable or absurd value is the default rather than a
    /// refusal: the interval is the one setting where carrying on once a minute
    /// is obviously better than not observing the stream at all, and a provider
    /// that wants it required declares it `required` in its `config_fields`,
    /// where the form refuses it in front of the admin.
    pub fn interval(config: &Attrs) -> Duration {
        let seconds = config
            .get(INTERVAL_FIELD)
            .and_then(|value| match value {
                Json::Number(n) => n.as_f64(),
                Json::String(s) => s.trim().parse::<f64>().ok(),
                _ => None,
            })
            // A value that is not a number at all is "unsaid", and unsaid is
            // the default. A zero or a negative one **was** said, and what it
            // says is "as fast as you can" — which the floor answers, rather
            // than quietly reading it as a minute.
            .filter(|s| s.is_finite())
            .unwrap_or(DEFAULT_INTERVAL_S as f64);
        Duration::from_secs_f64(seconds.max(0.0)).max(MIN_INTERVAL)
    }
}

#[async_trait]
impl StreamProvider for PollingProvider {
    fn name(&self) -> &str {
        &self.kind.name
    }

    fn label(&self) -> &str {
        &self.kind.label
    }

    fn description(&self) -> &str {
        &self.kind.description
    }

    fn config_spec(&self) -> Vec<FormField> {
        self.kind.config_spec.clone()
    }

    fn element_type(&self, config: &Attrs) -> Result<ElementType> {
        self.cached(config).ok_or_else(|| {
            Error::config(format!(
                "the element type of the stream provider `{}` from {} is declared as a function \
                 of the configuration, which only that module can evaluate, and nothing has \
                 resolved this configuration yet",
                self.kind.name,
                self.kind.source()
            ))
        })
    }

    async fn resolve_element_type(&self, config: &Attrs) -> Result<ElementType> {
        let ty = self.host.element_type(&self.kind, config).await?;
        // Checked here, not only on save: what comes back crossed a seam from
        // somebody else's JavaScript, and a `Json` type with no keys or a
        // `latin1` encoding must be a sentence rather than a stream that
        // delivers nothing and says nothing.
        ty.validate().map_err(|e| {
            Error::config(format!(
                "the stream provider `{}` from {} declared an element type this server cannot \
                 honour: {e}",
                self.kind.name,
                self.kind.source()
            ))
        })?;
        self.remember(config, &ty);
        Ok(ty)
    }

    fn kind(&self) -> StreamProviderKind {
        self.kind.clone()
    }

    async fn subscribe(
        &self,
        stream: &str,
        config: &Attrs,
        sink: Arc<dyn StreamSink>,
    ) -> Result<Subscription> {
        // Resolved *before* the loop starts, so a provider whose module has
        // gone away fails the subscribe — which the supervisor turns into
        // `failed` with the sentence and a backoff — rather than starting a
        // loop that can never decode anything.
        let element_type = self.resolve_element_type(config).await?;
        let interval = PollingProvider::interval(config);
        let kind = self.kind.clone();
        let host = Arc::clone(&self.host);
        let config = config.clone();
        let name = stream.to_owned();

        Ok(Subscription::spawn(move |mut stop| async move {
            let mut cursor = Json::Null;
            loop {
                // Between two pieces of work, with nothing to select on: a poll
                // that took a minute may have been stopped while it ran, and
                // delivering its elements afterwards would be a stopped stream
                // firing triggers.
                if stop.is_stopped() {
                    return;
                }
                match host.poll(&kind, &config, &cursor).await {
                    Ok(answer) => {
                        for raw in answer.elements {
                            match Element::decode(&element_type, RawPayload::Json(raw)) {
                                Ok(element) => sink.deliver(element),
                                // Counted and warned by the sink, never
                                // delivered (§11): one misbehaving item in a
                                // feed must not stop the rest of it.
                                Err(e) => sink.malformed(&sc_error::format_chain(&e)),
                            }
                        }
                        cursor = answer.cursor;
                    }
                    Err(e) => {
                        // End, so the supervisor's backoff services it (see the
                        // module docs). The line is worth printing because the
                        // status the admin sees is the supervisor's, and this
                        // is the only place the module's own sentence appears
                        // at the moment it happened.
                        eprintln!(
                            "feldspar: stream `{name}`: polling `{}` failed, so the subscription \
                             ended and will be retried: {}",
                            kind.name,
                            sc_error::format_chain(&e)
                        );
                        return;
                    }
                }
                tokio::select! {
                    () = stop.stopped() => return,
                    () = tokio::time::sleep(interval) => {}
                }
            }
        }))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use sc_types::BasicType;
    use serde_json::json;

    use super::*;
    use crate::element::ElementField;

    /// A sink that keeps what it was given.
    ///
    /// `testing::Collector`'s twin, written again here because that one is
    /// behind the `testing` feature and a unit test inside this crate builds
    /// without it — the same reason `registry.rs`'s tests declare their own
    /// provider.
    #[derive(Debug, Default)]
    struct Collector {
        elements: Mutex<Vec<Element>>,
        malformed: Mutex<Vec<String>>,
    }

    impl Collector {
        fn new() -> Collector {
            Collector::default()
        }

        fn values(&self) -> Vec<Json> {
            self.elements
                .lock()
                .unwrap()
                .iter()
                .map(|e| e.value.clone())
                .collect()
        }

        fn len(&self) -> usize {
            self.elements.lock().unwrap().len()
        }

        fn malformed(&self) -> Vec<String> {
            self.malformed.lock().unwrap().clone()
        }
    }

    impl StreamSink for Collector {
        fn deliver(&self, element: Element) {
            self.elements.lock().unwrap().push(element);
        }

        fn malformed(&self, reason: &str) {
            self.malformed.lock().unwrap().push(reason.to_owned());
        }
    }

    /// A host whose polls are a script: the nth call answers the nth entry, and
    /// an `Err` entry is a module that threw.
    struct ScriptedHost {
        ty: Result<ElementType>,
        answers: Vec<Result<Json>>,
        calls: AtomicUsize,
        cursors: Mutex<Vec<Json>>,
    }

    impl ScriptedHost {
        fn new(answers: Vec<Result<Json>>) -> Arc<ScriptedHost> {
            Arc::new(ScriptedHost {
                ty: Ok(ElementType::json([ElementField::new(
                    "title",
                    BasicType::Text,
                )])),
                answers,
                calls: AtomicUsize::new(0),
                cursors: Mutex::new(Vec::new()),
            })
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }

        fn cursors(&self) -> Vec<Json> {
            self.cursors.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl PollHost for ScriptedHost {
        async fn element_type(
            &self,
            _provider: &StreamProviderKind,
            _config: &Attrs,
        ) -> Result<ElementType> {
            self.ty
                .as_ref()
                .map(Clone::clone)
                .map_err(|e| Error::config(e.to_string()))
        }

        async fn poll(
            &self,
            _provider: &StreamProviderKind,
            _config: &Attrs,
            cursor: &Json,
        ) -> Result<PollAnswer> {
            self.cursors.lock().unwrap().push(cursor.clone());
            let index = self.calls.fetch_add(1, Ordering::SeqCst);
            match self.answers.get(index) {
                Some(Ok(answer)) => PollAnswer::read(answer.clone()),
                Some(Err(e)) => Err(Error::config(e.to_string())),
                // Past the end of the script: nothing new, same cursor.
                None => Ok(PollAnswer {
                    elements: Vec::new(),
                    cursor: cursor.clone(),
                }),
            }
        }
    }

    fn kind() -> StreamProviderKind {
        StreamProviderKind::new("poll_feed", "Polled feed", "a feed, polled")
            .module("@feldspar/rss")
    }

    fn polled(interval_s: f64) -> Attrs {
        let mut config = Attrs::new();
        config.insert(INTERVAL_FIELD.to_owned(), json!(interval_s));
        config
    }

    #[test]
    fn a_polls_answer_is_read_whole_or_as_a_bare_list() {
        let full = PollAnswer::read(json!({ "elements": [1, 2], "cursor": "abc" })).unwrap();
        assert_eq!(full.elements, vec![json!(1), json!(2)]);
        assert_eq!(full.cursor, json!("abc"));

        let bare = PollAnswer::read(json!([1])).unwrap();
        assert_eq!(bare.elements, vec![json!(1)]);
        assert_eq!(bare.cursor, Json::Null);

        // A poll with nothing new is the common answer and must not be an
        // error.
        let empty = PollAnswer::read(json!({ "cursor": 7 })).unwrap();
        assert!(empty.elements.is_empty());
        assert_eq!(empty.cursor, json!(7));

        let err = PollAnswer::read(json!({ "elements": 3 }))
            .unwrap_err()
            .to_string();
        assert!(err.contains("must be a list"), "{err}");
        let err = PollAnswer::read(json!("nope")).unwrap_err().to_string();
        assert!(err.contains("must answer"), "{err}");
    }

    #[test]
    fn the_interval_comes_from_the_configuration_and_has_a_floor() {
        assert_eq!(
            PollingProvider::interval(&Attrs::new()),
            Duration::from_secs(DEFAULT_INTERVAL_S)
        );
        assert_eq!(
            PollingProvider::interval(&polled(5.0)),
            Duration::from_secs(5)
        );
        // A string is what a form posts when the field was typed into.
        let mut typed = Attrs::new();
        typed.insert(INTERVAL_FIELD.to_owned(), json!("30"));
        assert_eq!(PollingProvider::interval(&typed), Duration::from_secs(30));
        // Zero is a loop with no wait in it, not "as fast as possible".
        assert_eq!(PollingProvider::interval(&polled(0.0)), MIN_INTERVAL);
        assert_eq!(
            PollingProvider::interval(&polled(0.001)),
            MIN_INTERVAL,
            "the floor holds under an absurdly small interval"
        );
    }

    #[tokio::test]
    async fn the_synchronous_element_type_answers_from_what_was_resolved() {
        let host = ScriptedHost::new(Vec::new());
        let provider = PollingProvider::new(kind(), host as Arc<dyn PollHost>);
        let config = polled(1.0);

        // Nothing resolved yet: a sentence saying so, naming the provider and
        // the module, rather than a guess.
        let err = provider.element_type(&config).unwrap_err().to_string();
        assert!(
            err.contains("poll_feed") && err.contains("@feldspar/rss"),
            "{err}"
        );

        let resolved = provider.resolve_element_type(&config).await.unwrap();
        assert_eq!(provider.element_type(&config).unwrap(), resolved);
        // And only for that configuration: another one is a different question
        // for the module, not the same answer.
        assert!(provider.element_type(&polled(2.0)).is_err());
    }

    #[tokio::test]
    async fn an_element_type_the_module_declares_badly_is_refused_naming_it() {
        let mut host = ScriptedHost::new(Vec::new());
        Arc::get_mut(&mut host).unwrap().ty = Ok(ElementType::json([]));
        let provider = PollingProvider::new(kind(), host as Arc<dyn PollHost>);
        let err = provider
            .resolve_element_type(&Attrs::new())
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("poll_feed"), "{err}");
        assert!(err.contains("at least one key"), "{err}");
    }

    #[tokio::test]
    async fn elements_are_polled_decoded_and_the_cursor_is_carried() {
        let host = ScriptedHost::new(vec![
            Ok(json!({ "elements": [{ "title": "one" }], "cursor": "c1" })),
            Ok(json!({ "elements": [{ "title": "two" }, { "title": 3 }], "cursor": "c2" })),
        ]);
        let provider = PollingProvider::new(kind(), Arc::clone(&host) as Arc<dyn PollHost>);
        let sink = Arc::new(Collector::new());
        let subscription = provider
            .subscribe("feed", &polled(0.1), sink.clone() as Arc<dyn StreamSink>)
            .await
            .unwrap();

        tokio::time::sleep(Duration::from_millis(250)).await;
        assert_eq!(
            sink.values(),
            vec![json!({ "title": "one" }), json!({ "title": "two" })]
        );
        // The third item is the wrong type for its declared key: counted, not
        // delivered, and the rest of the poll still arrived.
        let malformed = sink.malformed();
        assert_eq!(malformed.len(), 1, "{malformed:?}");
        assert!(malformed[0].contains("title"), "{malformed:?}");
        // The first poll starts from nothing, and each one afterwards is handed
        // what the last answered.
        let cursors = host.cursors();
        assert_eq!(cursors[0], Json::Null);
        assert_eq!(cursors[1], json!("c1"));
        assert_eq!(cursors[2], json!("c2"));

        drop(subscription);
        let after = host.calls();
        tokio::time::sleep(Duration::from_millis(250)).await;
        assert_eq!(host.calls(), after, "nothing is polled after a drop");
    }

    #[tokio::test]
    async fn a_poll_that_throws_ends_the_subscription_rather_than_spinning() {
        let host = ScriptedHost::new(vec![
            Ok(json!([{ "title": "one" }])),
            Err(Error::config("the feed could not be fetched")),
        ]);
        let provider = PollingProvider::new(kind(), Arc::clone(&host) as Arc<dyn PollHost>);
        let sink = Arc::new(Collector::new());
        let subscription = provider
            .subscribe("feed", &polled(0.1), sink.clone() as Arc<dyn StreamSink>)
            .await
            .unwrap();

        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            subscription.has_ended(),
            "a failing poll ends, leaving the supervisor to back off"
        );
        assert_eq!(sink.len(), 1, "what arrived before the failure was kept");
        // And it really stopped: no spinning behind the ended handle.
        let calls = host.calls();
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(host.calls(), calls);
    }

    #[tokio::test]
    async fn a_module_that_cannot_declare_its_element_type_fails_the_subscribe() {
        let mut host = ScriptedHost::new(Vec::new());
        Arc::get_mut(&mut host).unwrap().ty = Err(Error::config("the module is not loaded"));
        let provider = PollingProvider::new(kind(), host as Arc<dyn PollHost>);
        let err = provider
            .subscribe("feed", &Attrs::new(), Arc::new(Collector::new()))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("not loaded"), "{err}");
    }
}
