//! [`ScriptedProvider`] and [`Collector`]: a stream that says what it was told
//! to say, and somewhere to put what it said (TODO §13, task 1.5).
//!
//! Behind the `testing` **feature**, not `#[cfg(test)]`, and deliberately: the
//! supervisor is tested here, the trigger bridge and the observe sockets are
//! tested in `sc-server`, and a fake that only existed inside this crate's own
//! test build is one they could not reach. It is `sc-agent`'s `FakeProvider`
//! for streams, and it is a first-class part of the crate.
//!
//! **No test in this tree may need a broker.** This is what makes that
//! possible: every path through the supervisor — elements arriving, a
//! subscription that ends, a `subscribe` that fails until the third attempt, a
//! payload that does not match the declared element type — is a script rather
//! than a mosquitto in a container.
//!
//! ```no_run
//! # use sc_stream::testing::{Collector, ScriptedProvider};
//! # use sc_stream::{ElementType, RawPayload, StreamProvider};
//! # use sc_types::Attrs;
//! # use std::sync::Arc;
//! # async fn example() {
//! let provider = ScriptedProvider::new("scripted", ElementType::text())
//!     .elements([RawPayload::bytes(b"one".to_vec()), RawPayload::bytes(b"two".to_vec())]);
//! let sink = Arc::new(Collector::new());
//! let subscription = provider.subscribe("scripted", &Attrs::new(), sink.clone()).await.unwrap();
//! # let _ = subscription;
//! # }
//! ```

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use sc_error::{Error, Result};
use sc_types::{Attrs, FormField};

use crate::element::{ElementType, RawPayload};
use crate::envelope::Element;
use crate::provider::{StreamProvider, StreamSink};
use crate::subscription::Subscription;

/// A [`StreamSink`] that keeps what it was given.
///
/// The `Vec` §2 promises: it is what lets the supervisor be tested with no
/// broadcast channel, no socket and no trigger dispatcher behind it.
#[derive(Debug, Default)]
pub struct Collector {
    elements: Mutex<Vec<Element>>,
    malformed: Mutex<Vec<String>>,
}

impl Collector {
    /// A sink holding nothing.
    pub fn new() -> Collector {
        Collector::default()
    }

    /// Everything delivered so far, in order.
    pub fn elements(&self) -> Vec<Element> {
        self.elements.lock().map(|v| v.clone()).unwrap_or_default()
    }

    /// The `value` of everything delivered so far — what most assertions want.
    pub fn values(&self) -> Vec<serde_json::Value> {
        self.elements().into_iter().map(|e| e.value).collect()
    }

    /// How many elements have been delivered.
    pub fn len(&self) -> usize {
        self.elements.lock().map(|v| v.len()).unwrap_or(0)
    }

    /// Whether nothing has been delivered.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The reasons payloads were refused, in order.
    pub fn malformed(&self) -> Vec<String> {
        self.malformed.lock().map(|v| v.clone()).unwrap_or_default()
    }
}

impl StreamSink for Collector {
    fn deliver(&self, element: Element) {
        // A poisoned lock means a test has already failed somewhere else;
        // dropping the element is the quiet half of that, and a panic here
        // would bury the real failure.
        if let Ok(mut elements) = self.elements.lock() {
            elements.push(element);
        }
    }

    fn malformed(&self, reason: &str) {
        if let Ok(mut malformed) = self.malformed.lock() {
            malformed.push(reason.to_owned());
        }
    }
}

/// What a [`ScriptedProvider`] has done so far, shared with every subscription
/// it has handed out.
#[derive(Debug, Default)]
struct Script {
    subscribes: AtomicUsize,
    delivered: AtomicUsize,
}

/// A stream provider that delivers a script (§13).
///
/// Cheap to configure and deterministic: a list of payloads, optionally on a
/// timer, optionally failing the first *n* `subscribe` calls so the
/// supervisor's backoff is a test rather than a hope.
pub struct ScriptedProvider {
    name: String,
    description: String,
    config_spec: Vec<FormField>,
    element_type: ElementType,
    script: Vec<RawPayload>,
    interval: Option<Duration>,
    repeat: bool,
    ends: bool,
    fail_first: usize,
    state: Arc<Script>,
}

impl ScriptedProvider {
    /// A provider of this name, whose elements are of this type and whose
    /// script is empty.
    pub fn new(name: impl Into<String>, element_type: ElementType) -> ScriptedProvider {
        ScriptedProvider {
            name: name.into(),
            description: "a scripted stream, for tests".to_owned(),
            config_spec: Vec::new(),
            element_type,
            script: Vec::new(),
            interval: None,
            repeat: false,
            ends: false,
            fail_first: 0,
            state: Arc::new(Script::default()),
        }
    }

    /// The payloads it delivers, in order.
    pub fn elements(mut self, script: impl IntoIterator<Item = RawPayload>) -> ScriptedProvider {
        self.script = script.into_iter().collect();
        self
    }

    /// The payloads it delivers, written as JSON — the common case.
    pub fn json_elements(
        self,
        script: impl IntoIterator<Item = serde_json::Value>,
    ) -> ScriptedProvider {
        let script: Vec<RawPayload> = script.into_iter().map(RawPayload::Json).collect();
        self.elements(script)
    }

    /// Deliver one element every `interval` rather than all of them at once.
    pub fn every(mut self, interval: Duration) -> ScriptedProvider {
        self.interval = Some(interval);
        self
    }

    /// Start the script again when it runs out, so the stream never goes quiet
    /// — what a test of "stop this" or of the element-rate cap needs.
    pub fn repeating(mut self) -> ScriptedProvider {
        self.repeat = true;
        self
    }

    /// **End** the subscription when the script runs out, rather than staying
    /// connected and silent.
    ///
    /// The broker-hung-up case: the supervisor notices
    /// ([`Subscription::has_ended`](crate::Subscription::has_ended)) and
    /// restarts it (§6).
    pub fn ending(mut self) -> ScriptedProvider {
        self.ends = true;
        self
    }

    /// Fail the first `n` calls to `subscribe`, then succeed.
    ///
    /// The backoff path: the supervisor must keep the stream `failed` with the
    /// error, count attempts, and still be running when the broker comes back.
    pub fn failing_first(mut self, n: usize) -> ScriptedProvider {
        self.fail_first = n;
        self
    }

    /// The settings this provider declares — for a test that needs a secret to
    /// survive an edit, or a configuration to validate against something.
    pub fn config(mut self, spec: Vec<FormField>) -> ScriptedProvider {
        self.config_spec = spec;
        self
    }

    /// Say something else in the picker.
    pub fn description(mut self, description: impl Into<String>) -> ScriptedProvider {
        self.description = description.into();
        self
    }

    /// How many times `subscribe` has been called — the supervisor's restarts,
    /// counted from outside.
    pub fn subscribes(&self) -> usize {
        self.state.subscribes.load(Ordering::SeqCst)
    }

    /// How many elements this provider has handed to a sink.
    pub fn delivered(&self) -> usize {
        self.state.delivered.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl StreamProvider for ScriptedProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn config_spec(&self) -> Vec<FormField> {
        self.config_spec.clone()
    }

    fn element_type(&self, _config: &Attrs) -> Result<ElementType> {
        Ok(self.element_type.clone())
    }

    async fn subscribe(
        &self,
        _stream: &str,
        _config: &Attrs,
        sink: Arc<dyn StreamSink>,
    ) -> Result<Subscription> {
        let attempt = self.state.subscribes.fetch_add(1, Ordering::SeqCst) + 1;
        if attempt <= self.fail_first {
            return Err(Error::msg(format!(
                "scripted failure on subscribe {attempt} of {}",
                self.fail_first
            )));
        }

        let ty = self.element_type.clone();
        let script = self.script.clone();
        let interval = self.interval;
        let repeat = self.repeat;
        let ends = self.ends;
        let state = Arc::clone(&self.state);

        Ok(Subscription::spawn(move |mut stop| async move {
            loop {
                for raw in &script {
                    if let Some(interval) = interval {
                        tokio::select! {
                            _ = stop.stopped() => return,
                            _ = tokio::time::sleep(interval) => {}
                        }
                    } else if stop.is_stopped() {
                        return;
                    }
                    match Element::decode(&ty, raw.clone()) {
                        Ok(element) => {
                            state.delivered.fetch_add(1, Ordering::SeqCst);
                            sink.deliver(element);
                        }
                        // Exactly what a real provider does with a payload that
                        // does not match the declaration: count it, do not
                        // deliver it (§11).
                        Err(e) => sink.malformed(&e.to_string()),
                    }
                }
                if repeat {
                    continue;
                }
                if ends {
                    // The broker hung up. The supervisor sees a subscription
                    // that has ended and restarts it.
                    return;
                }
                // Connected and quiet, until somebody drops the handle.
                stop.stopped().await;
                return;
            }
        }))
    }
}
