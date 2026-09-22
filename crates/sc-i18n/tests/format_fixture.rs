//! `fixtures/format.json`, run against `sc_i18n::format` (proposal §2).
//!
//! The Rust half of a corpus the TypeScript half also runs (task 3.4, over the
//! generated `messages.ts`). The point of a *file* rather than two sets of unit
//! tests is that two implementations of one thing disagree by the third bug
//! fixed in one of them: a case added here is a case both halves are held to,
//! and neither can quietly drift.

use sc_i18n::{Arg, format};
use serde_json::Value as Json;

#[test]
fn every_case_in_the_shared_corpus() {
    let raw = include_str!("../fixtures/format.json");
    let doc: Json = serde_json::from_str(raw).expect("fixtures/format.json is valid JSON");
    let cases = doc["cases"].as_array().expect("`cases` is an array");
    assert!(cases.len() > 20, "the corpus should not have shrunk");

    for case in cases {
        let name = case["name"].as_str().unwrap_or("<unnamed>");
        let message = case["message"].as_str().expect("`message` is a string");
        let expected = case["expected"].as_str().expect("`expected` is a string");
        let args: Vec<(&str, Arg)> = case["args"]
            .as_object()
            .expect("`args` is an object")
            .iter()
            .map(|(key, value)| (key.as_str(), arg(value)))
            .collect();

        assert_eq!(format(message, &args), expected, "case: {name}");
    }
}

/// One fixture argument as an [`Arg`]. The corpus uses the three JSON shapes a
/// message argument can have, and an integer must come out as `3` rather than
/// `3.0` — which is exactly the kind of difference this file exists to pin.
fn arg(value: &Json) -> Arg {
    match value {
        Json::String(s) => Arg::from(s.as_str()),
        Json::Number(n) if n.is_i64() => Arg::from(n.as_i64().unwrap_or_default()),
        Json::Number(n) => Arg::from(n.as_f64().unwrap_or_default()),
        Json::Bool(b) => Arg::from(*b),
        other => panic!("a fixture argument should be a string, number or boolean, got {other}"),
    }
}
