//! A stream's secrets: redacted on the way out to a form, restored on the way
//! back in (TODO task 2.3).
//!
//! A stream's configuration is whatever its provider declared, and one of those
//! declarations is a **password**: MQTT's, an API token on a polled feed, the
//! bearer a queue wants. So this entity meets the same round trip every
//! spec-configured record in this tree meets — an LLM provider's API key, a file
//! store's credentials, a module's settings — and meets it the same way:
//! [`sc_types::redact_attrs`] replaces a [`secret`](sc_types::FormField::secret)
//! value with [`SECRET_SENTINEL`](sc_types::SECRET_SENTINEL) on the way out, and
//! [`sc_types::merge_secrets`] puts the stored value back wherever the sentinel
//! comes back untouched.
//!
//! The half that is load-bearing is the second one. Masking a field that a form
//! writes back is the classic way to destroy the thing you were protecting: an
//! admin opens a stream to fix a topic filter, saves, and the row now holds
//! `••••••••` where the broker password was. The broker then refuses the
//! connection, the supervisor retries it forever, and nothing anywhere says that
//! a *save* is what broke it.
//!
//! ## Why the merge is inside `save_stream` rather than in the handler
//!
//! Every other record in this tree merges in its HTTP handler, because that is
//! where a submitted body arrives. A stream does it in
//! [`save_stream`](crate::save_stream) — the one door every write goes through —
//! for two reasons that are specific to this entity:
//!
//! - **Validation must not judge the mask.** `save_stream` validates first
//!   (§2.2), and a provider's own `validate` reads its settings: a password
//!   whose shape it checks, a token whose prefix it recognises. Handed
//!   `••••••••` it would refuse a configuration that is in fact fine, or accept
//!   one that is not.
//! - **A stream that is wrong connects anyway.** The sentinel reaching the row
//!   is not a form that will not open; it is a subscription failing against a
//!   live broker at 3am. The narrowest place to make that impossible is the
//!   function that writes the row.
//!
//! [`redacted_stream`] is still the outward half and is called by whatever
//! serialises a stream (task 6.1's endpoints), because this crate has no idea
//! what JSON looks like.
//!
//! ## An unknown provider redacts nothing, and that is not a leak
//!
//! A stream whose provider came from a module that has been uninstalled must
//! stay listable and editable — editing it *is* the repair. So a provider that
//! cannot be resolved leaves the configuration alone rather than failing the
//! read, exactly as a file store's `shown_config` and an LLM provider's
//! `redacted_provider_config` do. Nothing escapes that way: no spec means no
//! field was ever declared secret, so there is no secret to know about.

use sc_types::{Attrs, merge_secrets, redact_attrs};

use crate::registry::StreamRegistry;
use crate::stream::Stream;

/// `configuration` with the `provider`'s [`secret`](sc_types::FormField::secret)
/// settings replaced by the sentinel.
///
/// Only keys that are *present* are masked ([`redact_attrs`]'s rule), so the
/// form can still tell "no password set yet" from "a password it may not see".
pub fn redact_configuration(
    registry: &StreamRegistry,
    provider: &str,
    configuration: &Attrs,
) -> Attrs {
    match registry.get(provider.trim()) {
        Some(provider) => redact_attrs(&provider.config_spec(), configuration),
        // See the module docs: no spec, nothing declared secret.
        None => configuration.clone(),
    }
}

/// The stream as a form or an API response should see it: itself, with its
/// configuration [redacted](redact_configuration).
///
/// The read-for-display path §2.3 asks for. Everything that serialises a stream
/// outward goes through this, so an endpoint added later cannot return a
/// password without building the JSON by hand to do it.
pub fn redacted_stream(registry: &StreamRegistry, stream: &Stream) -> Stream {
    Stream {
        configuration: redact_configuration(registry, &stream.provider, &stream.configuration),
        ..stream.clone()
    }
}

/// The reverse: `submitted`'s sentinels replaced by what `stored` holds, so a
/// password survives an edit that did not retype it.
///
/// `stored` is `None` on a **create**, and then a submitted sentinel is dropped
/// rather than stored — there is nothing behind the mask, and a provider that
/// declared the field `required` refuses the save with the admin still looking
/// at the form. Storing the mask would instead produce a stream that connects
/// with `••••••••` as its password.
pub fn restore_secrets(
    registry: &StreamRegistry,
    stored: Option<&Stream>,
    submitted: &Stream,
) -> Stream {
    let Some(provider) = registry.get(submitted.provider.trim()) else {
        return submitted.clone();
    };
    // A configuration is only comparable against what is stored for the *same*
    // provider: repointing a stream from one provider to another means the old
    // row's settings are a different vocabulary, and merging a sentinel into
    // them would take a password from a field that happens to share a name.
    let stored = stored
        .filter(|s| s.provider.trim() == submitted.provider.trim())
        .map(|s| s.configuration.clone())
        .unwrap_or_default();
    Stream {
        configuration: merge_secrets(&provider.config_spec(), &stored, &submitted.configuration),
        ..submitted.clone()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use sc_error::Result;
    use sc_types::{BasicType, FormField, SECRET_SENTINEL, TypeRef};
    use serde_json::json;

    use super::*;
    use crate::element::ElementType;
    use crate::provider::{StreamProvider, StreamSink};
    use crate::stream::StreamId;
    use crate::subscription::Subscription;

    /// A provider that wants a password, which is the whole point of this
    /// module — plus a setting that is not secret, so a test can tell the two
    /// treatments apart.
    struct WithSecret(&'static str);

    #[async_trait]
    impl StreamProvider for WithSecret {
        fn name(&self) -> &str {
            self.0
        }
        fn description(&self) -> &str {
            "a provider with a password, for tests"
        }
        fn config_spec(&self) -> Vec<FormField> {
            vec![
                FormField::new("topic", TypeRef::Basic(BasicType::Text)),
                FormField::new("username", TypeRef::Basic(BasicType::Text)),
                FormField::new("password", TypeRef::Basic(BasicType::Text)).secret(),
            ]
        }
        fn element_type(&self, _config: &Attrs) -> Result<ElementType> {
            Ok(ElementType::text())
        }
        async fn subscribe(
            &self,
            _config: &Attrs,
            _sink: Arc<dyn StreamSink>,
        ) -> Result<Subscription> {
            Ok(Subscription::spawn(|mut stop| async move {
                stop.stopped().await;
            }))
        }
    }

    fn registry() -> StreamRegistry {
        let mut registry = StreamRegistry::new();
        registry
            .register(Arc::new(WithSecret("secretive")))
            .unwrap();
        registry.register(Arc::new(WithSecret("other"))).unwrap();
        registry
    }

    fn stored() -> Stream {
        Stream::with_id(StreamId(uuid::Uuid::nil()), "boiler", "secretive")
            .config("topic", "house/boiler/#")
            .config("username", "sensor")
            .config("password", "hunter2")
    }

    #[test]
    fn a_secret_is_masked_on_the_way_out_and_nothing_else_is() {
        let shown = redacted_stream(&registry(), &stored());
        assert_eq!(shown.configuration["password"], json!(SECRET_SENTINEL));
        assert_eq!(shown.configuration["username"], json!("sensor"));
        assert_eq!(shown.configuration["topic"], json!("house/boiler/#"));
        assert!(
            !serde_json::Value::Object(shown.configuration.clone())
                .to_string()
                .contains("hunter2"),
            "no part of the password may leave"
        );
        // And the rest of the stream is untouched, so this is the value the
        // form renders.
        assert_eq!(shown.name, "boiler");
        assert_eq!(shown.provider, "secretive");
    }

    #[test]
    fn an_unset_secret_stays_unset_so_the_form_can_tell_the_difference() {
        let stream = Stream::with_id(StreamId(uuid::Uuid::nil()), "boiler", "secretive")
            .config("username", "sensor");
        let shown = redacted_stream(&registry(), &stream);
        assert!(!shown.configuration.contains_key("password"));
    }

    #[test]
    fn a_password_survives_an_edit_that_never_saw_it() {
        let registry = registry();
        let stored = stored();

        // The admin opens the form: this is everything the browser knows.
        let shown = redacted_stream(&registry, &stored);
        // They change the topic and save, sending the mask back untouched.
        let submitted = shown.config("topic", "house/#");

        let saved = restore_secrets(&registry, Some(&stored), &submitted);
        assert_eq!(saved.configuration["password"], json!("hunter2"));
        assert_eq!(saved.configuration["topic"], json!("house/#"));
    }

    #[test]
    fn a_retyped_password_is_the_one_that_is_kept() {
        let registry = registry();
        let stored = stored();
        let submitted = redacted_stream(&registry, &stored).config("password", "correct-horse");
        let saved = restore_secrets(&registry, Some(&stored), &submitted);
        assert_eq!(saved.configuration["password"], json!("correct-horse"));
    }

    #[test]
    fn a_create_drops_the_sentinel_rather_than_storing_the_mask() {
        let registry = registry();
        let submitted = Stream::with_id(StreamId(uuid::Uuid::nil()), "boiler", "secretive")
            .config("password", SECRET_SENTINEL);
        let saved = restore_secrets(&registry, None, &submitted);
        assert!(
            !saved.configuration.contains_key("password"),
            "there is nothing behind the mask on a create"
        );
    }

    #[test]
    fn repointing_a_stream_at_another_provider_does_not_carry_the_password_over() {
        let registry = registry();
        let stored = stored();
        // Same field name, different provider: the settings are a different
        // vocabulary, so there is nothing to restore from.
        let submitted = Stream::with_id(StreamId(uuid::Uuid::nil()), "boiler", "other")
            .config("password", SECRET_SENTINEL);
        let saved = restore_secrets(&registry, Some(&stored), &submitted);
        assert!(!saved.configuration.contains_key("password"));
    }

    #[test]
    fn an_unresolvable_provider_is_passed_through_both_ways() {
        let registry = registry();
        let stream = Stream::with_id(StreamId(uuid::Uuid::nil()), "boiler", "from_a_gone_module")
            .config("password", "hunter2");
        // Still listable and editable, which is the repair — and no spec means
        // nothing was ever declared secret.
        assert_eq!(
            redacted_stream(&registry, &stream).configuration,
            stream.configuration
        );
        assert_eq!(
            restore_secrets(&registry, Some(&stream), &stream).configuration,
            stream.configuration
        );
    }
}
