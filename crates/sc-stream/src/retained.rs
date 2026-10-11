//! [`Retained`]: the latest element per topic, for a stream whose provider asks
//! for [`Replay::Latest`](crate::Replay::Latest) (TODO.md "Live updates" L2.3).
//!
//! MQTT's *retained* message, in memory: a page that opens — or reloads —
//! halfway through a long job is handed the job's current progress straight
//! away instead of a blank bar until the next publish. Two bounds keep it a
//! cache and not a store:
//!
//! - **A time to live.** An element older than the TTL is not handed out, and
//!   is forgotten the next time anything is published. "The job is 40% done"
//!   from yesterday is not the current progress of anything.
//! - **A cap on topics per stream.** A per-user stream with a million users
//!   would otherwise keep a million envelopes; past the cap the topic published
//!   to longest ago is forgotten first.
//!
//! Kept by the [`RunningStream`](crate::RunningStream) under the same lock as its
//! replay ring and broadcast send, so a live subscriber is handed exactly the
//! retained elements published before it joined and receives every one after —
//! never both, never neither. The live hub's fan-out keeps its own copy, fed in
//! the same order, for the reason it keeps its own ring.
//!
//! Plain data, generic over nothing but the clock it is handed, so it is tested
//! here as the small data structure it is.

use std::collections::{HashMap, VecDeque};
use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::envelope::Envelope;

/// The latest envelope per topic, bounded by age and by count.
#[derive(Debug, Clone)]
pub struct Retained {
    ttl: Duration,
    max_topics: usize,
    latest: HashMap<Option<String>, Envelope>,
    /// Topics in the order they were last published to, oldest first, with the
    /// instant of that publish. An entry whose instant no longer matches the
    /// topic's envelope is stale — the topic was published to again since — and
    /// is skipped when it reaches the front. That keeps eviction amortised
    /// O(1) rather than a scan of every topic on every publish.
    order: VecDeque<(Option<String>, DateTime<Utc>)>,
}

impl Retained {
    /// Keep the latest element per topic for `ttl`, at most `max_topics` of
    /// them.
    pub fn new(ttl: Duration, max_topics: usize) -> Retained {
        Retained {
            ttl,
            max_topics: max_topics.max(1),
            latest: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    /// Remember `envelope` as its topic's latest, forgetting whatever has
    /// expired by `now` and, past the cap, the topic published to longest ago.
    pub fn keep(&mut self, envelope: Envelope, now: DateTime<Utc>) {
        let topic = envelope.topic.clone();
        self.order.push_back((topic.clone(), envelope.received_at));
        self.latest.insert(topic, envelope);
        self.forget_expired(now);
        while self.latest.len() > self.max_topics {
            if !self.pop_oldest() {
                break;
            }
        }
        // The order queue holds one entry per publish, so a topic published to
        // again and again grows it; compact it once it is mostly stale.
        if self.order.len() > self.latest.len().saturating_mul(2).max(64) {
            let latest = &self.latest;
            self.order.retain(|(topic, at)| {
                latest
                    .get(topic)
                    .is_some_and(|envelope| envelope.received_at == *at)
            });
        }
    }

    /// The latest element on `topic`, if there is one and it is still fresh at
    /// `now`.
    pub fn get(&self, topic: Option<&str>, now: DateTime<Utc>) -> Option<&Envelope> {
        let envelope = self.latest.get(&topic.map(str::to_owned))?;
        self.fresh(envelope, now).then_some(envelope)
    }

    /// Every fresh element, one per topic — what a copy of this is seeded
    /// from.
    pub fn all(&self, now: DateTime<Utc>) -> Vec<Envelope> {
        let mut out: Vec<Envelope> = self
            .latest
            .values()
            .filter(|envelope| self.fresh(envelope, now))
            .cloned()
            .collect();
        out.sort_by_key(|envelope| envelope.received_at);
        out
    }

    /// How many topics have an element kept, fresh or not yet forgotten.
    pub fn len(&self) -> usize {
        self.latest.len()
    }

    /// Whether nothing is kept.
    pub fn is_empty(&self) -> bool {
        self.latest.is_empty()
    }

    fn fresh(&self, envelope: &Envelope, now: DateTime<Utc>) -> bool {
        match chrono::Duration::from_std(self.ttl) {
            Ok(ttl) => envelope.received_at + ttl > now,
            // A TTL too long to represent never expires.
            Err(_) => true,
        }
    }

    /// Forget every topic whose latest element has expired.
    fn forget_expired(&mut self, now: DateTime<Utc>) {
        while let Some((topic, at)) = self.order.front() {
            let current = self.latest.get(topic);
            match current {
                // Stale: the topic was published to again, or forgotten.
                Some(envelope) if envelope.received_at != *at => {
                    self.order.pop_front();
                }
                None => {
                    self.order.pop_front();
                }
                Some(envelope) => {
                    if self.fresh(envelope, now) {
                        break;
                    }
                    let topic = topic.clone();
                    self.order.pop_front();
                    self.latest.remove(&topic);
                }
            }
        }
    }

    /// Forget the topic published to longest ago. `false` when there is
    /// nothing left to forget.
    fn pop_oldest(&mut self) -> bool {
        while let Some((topic, at)) = self.order.pop_front() {
            if self
                .latest
                .get(&topic)
                .is_some_and(|envelope| envelope.received_at == at)
            {
                self.latest.remove(&topic);
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::Element;
    use serde_json::json;

    fn at(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000 + seconds, 0).unwrap()
    }

    fn on(topic: &str, value: i64, seconds: i64) -> Envelope {
        Element::new(json!({ "n": value }))
            .topic(topic)
            .into_envelope("job_status", at(seconds))
    }

    #[test]
    fn the_latest_element_per_topic_is_kept_and_replaced() {
        let mut retained = Retained::new(Duration::from_secs(60), 100);
        retained.keep(on("alice", 1, 0), at(0));
        retained.keep(on("bob", 2, 1), at(1));
        retained.keep(on("alice", 3, 2), at(2));
        assert_eq!(retained.get(Some("alice"), at(3)).unwrap().value["n"], 3);
        assert_eq!(retained.get(Some("bob"), at(3)).unwrap().value["n"], 2);
        assert!(retained.get(Some("carol"), at(3)).is_none());
        assert!(retained.get(None, at(3)).is_none());
        assert_eq!(retained.len(), 2);
    }

    #[test]
    fn an_expired_element_is_not_handed_out_and_is_forgotten_on_the_next_publish() {
        let mut retained = Retained::new(Duration::from_secs(10), 100);
        retained.keep(on("alice", 1, 0), at(0));
        assert!(retained.get(Some("alice"), at(9)).is_some());
        assert!(
            retained.get(Some("alice"), at(10)).is_none(),
            "the TTL is exclusive"
        );
        assert!(retained.all(at(11)).is_empty());
        retained.keep(on("bob", 2, 11), at(11));
        assert_eq!(retained.len(), 1, "alice's expired element was forgotten");
    }

    #[test]
    fn past_the_cap_the_topic_published_to_longest_ago_goes_first() {
        let mut retained = Retained::new(Duration::from_secs(600), 2);
        retained.keep(on("a", 1, 0), at(0));
        retained.keep(on("b", 1, 1), at(1));
        // `a` is published to again, so `b` is now the oldest.
        retained.keep(on("a", 2, 2), at(2));
        retained.keep(on("c", 1, 3), at(3));
        assert_eq!(retained.len(), 2);
        assert!(retained.get(Some("b"), at(4)).is_none());
        assert_eq!(retained.get(Some("a"), at(4)).unwrap().value["n"], 2);
        assert!(retained.get(Some("c"), at(4)).is_some());
    }

    #[test]
    fn a_topic_published_to_a_thousand_times_does_not_grow_the_order_queue() {
        let mut retained = Retained::new(Duration::from_secs(600), 10);
        for i in 0..1000 {
            retained.keep(on("busy", i, i), at(i));
        }
        assert_eq!(retained.len(), 1);
        assert!(retained.order.len() <= 64, "{}", retained.order.len());
        assert_eq!(
            retained.get(Some("busy"), at(1000)).unwrap().value["n"],
            999
        );
    }

    #[test]
    fn all_is_every_fresh_element_oldest_first() {
        let mut retained = Retained::new(Duration::from_secs(5), 10);
        retained.keep(on("old", 1, 0), at(0));
        retained.keep(on("b", 1, 3), at(3));
        retained.keep(on("a", 1, 4), at(4));
        let topics: Vec<Option<String>> =
            retained.all(at(6)).into_iter().map(|e| e.topic).collect();
        assert_eq!(topics, vec![Some("b".to_owned()), Some("a".to_owned())]);
    }
}
