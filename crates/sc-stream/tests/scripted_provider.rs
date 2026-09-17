//! The scripted provider, which every later phase's test rides on (task 1.5).
//!
//! An integration test rather than a unit one because the fake lives behind the
//! `testing` feature (see `sc_stream::testing`), and that feature is turned on
//! for this crate's test build only here.

use std::sync::Arc;
use std::time::Duration;

use sc_stream::testing::{Collector, ScriptedProvider};
use sc_stream::{ElementField, ElementType, RawPayload, StreamProvider, StreamRegistry};
use sc_types::{Attrs, BasicType, FormField};
use serde_json::json;

/// Long enough for a spawned task to make progress on any machine that can run
/// the rest of this suite; short enough that the file still runs in
/// milliseconds.
const SETTLE: Duration = Duration::from_millis(80);

async fn settle() {
    tokio::time::sleep(SETTLE).await;
}

fn reading() -> ElementType {
    ElementType::json([
        ElementField::new("n", BasicType::Int).required(),
        ElementField::new("unit", BasicType::Text),
    ])
}

#[tokio::test]
async fn elements_arrive_in_the_order_the_script_wrote_them() {
    let provider = ScriptedProvider::new("scripted", reading()).json_elements([
        json!({"n": 1}),
        json!({"n": 2}),
        json!({"n": 3}),
    ]);
    let sink = Arc::new(Collector::new());

    let subscription = provider
        .subscribe(&Attrs::new(), Arc::clone(&sink) as Arc<_>)
        .await
        .unwrap();
    settle().await;

    assert_eq!(
        sink.values()
            .iter()
            .map(|v| v["n"].as_i64().unwrap())
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    // The declared-but-absent key is a null, not a missing key (§4).
    assert!(sink.values()[0].as_object().unwrap().contains_key("unit"));
    assert_eq!(provider.delivered(), 3);
    assert_eq!(provider.subscribes(), 1);
    // A script that has run out but does not say it ends stays connected and
    // quiet — a broker with nothing to say, not a broker that hung up.
    assert!(!subscription.has_ended());
}

#[tokio::test]
async fn dropping_the_subscription_stops_the_elements() {
    let provider = ScriptedProvider::new("scripted", ElementType::text())
        .elements([RawPayload::bytes(b"tick".to_vec())])
        .every(Duration::from_millis(5))
        .repeating();
    let sink = Arc::new(Collector::new());

    let subscription = provider
        .subscribe(&Attrs::new(), Arc::clone(&sink) as Arc<_>)
        .await
        .unwrap();
    settle().await;
    assert!(!sink.is_empty(), "the timer delivered something");

    drop(subscription);
    settle().await;
    let after_stop = sink.len();
    settle().await;
    assert_eq!(
        sink.len(),
        after_stop,
        "nothing arrives after the handle is dropped"
    );
}

#[tokio::test]
async fn the_nth_subscribe_fails_on_demand() {
    let provider = ScriptedProvider::new("scripted", ElementType::text())
        .elements([RawPayload::bytes(b"hello".to_vec())])
        .failing_first(2);
    let sink = Arc::new(Collector::new());

    for attempt in 1..=2 {
        let err = provider
            .subscribe(&Attrs::new(), Arc::clone(&sink) as Arc<_>)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains(&format!("subscribe {attempt}")), "{err}");
    }
    // The third attempt connects — which is the shape the supervisor's backoff
    // is tested against in Phase 3: fail, wait, fail, wait, succeed.
    let subscription = provider
        .subscribe(&Attrs::new(), Arc::clone(&sink) as Arc<_>)
        .await
        .unwrap();
    settle().await;
    assert_eq!(sink.values(), vec![json!("hello")]);
    assert_eq!(provider.subscribes(), 3);
    drop(subscription);
}

#[tokio::test]
async fn a_subscription_that_ends_says_so() {
    let provider = ScriptedProvider::new("scripted", ElementType::text())
        .elements([RawPayload::bytes(b"bye".to_vec())])
        .ending();
    let sink = Arc::new(Collector::new());

    let subscription = provider
        .subscribe(&Attrs::new(), Arc::clone(&sink) as Arc<_>)
        .await
        .unwrap();
    settle().await;
    assert_eq!(sink.len(), 1);
    // The broker hung up: the supervisor restarts it with backoff (§6), which
    // it can only do because this is visible from the handle.
    assert!(subscription.has_ended());
}

#[tokio::test]
async fn a_payload_that_does_not_match_the_declaration_is_counted_not_delivered() {
    let provider = ScriptedProvider::new("scripted", reading()).elements([
        RawPayload::json(json!({"n": 1})),
        RawPayload::bytes(b"not json at all".to_vec()),
        RawPayload::json(json!({"unit": "C"})),
        RawPayload::json(json!({"n": 2})),
    ]);
    let sink = Arc::new(Collector::new());

    let subscription = provider
        .subscribe(&Attrs::new(), Arc::clone(&sink) as Arc<_>)
        .await
        .unwrap();
    settle().await;

    assert_eq!(sink.len(), 2, "only the two well-formed payloads arrived");
    assert_eq!(provider.delivered(), 2);
    let refused = sink.malformed();
    assert_eq!(refused.len(), 2);
    assert!(refused[0].contains("not JSON"), "{:?}", refused[0]);
    assert!(refused[1].contains("`n`"), "{:?}", refused[1]);
    drop(subscription);
}

#[tokio::test]
async fn a_scripted_provider_is_a_provider_like_any_other() {
    let provider = ScriptedProvider::new("scripted", reading())
        .description("a temperature sensor that is not there")
        .config(vec![
            FormField::new("host", BasicType::Text).required(),
            FormField::new("password", BasicType::Text).secret(),
        ]);

    let mut registry = StreamRegistry::new();
    registry.register(Arc::new(provider)).unwrap();

    let kinds = registry.kinds();
    assert_eq!(kinds.len(), 1);
    assert_eq!(kinds[0].name, "scripted");
    assert_eq!(kinds[0].label, "scripted");
    assert_eq!(kinds[0].source(), "the built-in providers");
    assert_eq!(kinds[0].config_spec.len(), 2);
    assert!(
        kinds[0].config_spec[1].secret,
        "the secret survives the kind"
    );

    let provider = registry.require("scripted").unwrap();
    assert_eq!(
        provider.element_type(&Attrs::new()).unwrap().keys().len(),
        2
    );
    // The default `validate` is "can this configuration even produce an element
    // type", which a scripted provider's always can.
    provider.validate(&Attrs::new()).unwrap();
}
