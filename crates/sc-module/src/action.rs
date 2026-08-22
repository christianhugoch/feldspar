//! A module's action, as an [`Action`].
//!
//! The whole of what makes a module's action work here: the manifest says what
//! it is called and what it is configured with, and `run` marshals an
//! [`ActionContext`] into v1's argument object and sends it to the host.
//!
//! **v1's argument object is a wire contract**, not a convenience: a plugin's
//! `run` destructures `{ row, table, configuration, user }` and has done for
//! years, and every one of those names is what a module already reads. So the
//! translation is written out here, once, and the names do not change.

use std::sync::Arc;

use sc_action::{Action, ActionContext, Event};
use sc_error::Result;
use sc_types::{Attrs, FormField};
use serde_json::{Map, Value as Json, json};

use crate::host::ModuleHost;

/// One action supplied by one module.
pub struct ModuleAction {
    /// The package the action came from — what the Modules tab attributes it to,
    /// and what the host dispatches on.
    module: String,
    /// The action's own name, unqualified, exactly as v1 spells it.
    name: String,
    /// The description the module gave, or one naming the module.
    description: String,
    /// The settings, translated from v1's `configFields` at load
    /// ([`crate::spec`]).
    config_spec: Vec<FormField>,
    /// The host to run it in.
    host: Arc<ModuleHost>,
}

impl ModuleAction {
    /// An action of `module`, as its manifest describes it.
    pub fn new(
        module: impl Into<String>,
        name: impl Into<String>,
        description: impl Into<String>,
        config_spec: Vec<FormField>,
        host: Arc<ModuleHost>,
    ) -> ModuleAction {
        let module = module.into();
        let name = name.into();
        let described = description.into();
        let description = if described.trim().is_empty() {
            format!("{name}, from the module {module}")
        } else {
            described
        };
        ModuleAction {
            module,
            name,
            description,
            config_spec,
            host,
        }
    }

    /// The package this action came from.
    pub fn module(&self) -> &str {
        &self.module
    }
}

#[async_trait::async_trait]
impl Action for ModuleAction {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn config_spec(&self) -> Vec<FormField> {
        self.config_spec.clone()
    }

    async fn run(&self, ctx: &mut ActionContext<'_>) -> Result<Json> {
        let args = v1_arguments(ctx.event, ctx.config, ctx.trigger);
        self.host.run(&self.module, &self.name, args).await
    }
}

/// The object a v1 `run` is called with.
///
/// `table` is an object with a `name` rather than v1's `Table` model: the model
/// is one of the APIs that is stubbed in this milestone, and handing over
/// something that *looks* like one but answers nothing would be the silent
/// failure this system refuses. A module that only needs the name — which is
/// most of them — gets it.
fn v1_arguments(event: &Event, config: &Attrs, trigger: &str) -> Json {
    let mut args = Map::new();
    args.insert("row".into(), event.row.clone().unwrap_or(Json::Null));
    args.insert(
        "old_row".into(),
        event.old_row.clone().unwrap_or(Json::Null),
    );
    args.insert(
        "table".into(),
        match &event.channel {
            Some(name) => json!({ "name": name }),
            None => Json::Null,
        },
    );
    args.insert(
        "channel".into(),
        match &event.channel {
            Some(name) => Json::String(name.clone()),
            None => Json::Null,
        },
    );
    args.insert("configuration".into(), Json::Object(config.clone()));
    args.insert("user".into(), event.user.clone().unwrap_or(Json::Null));
    args.insert("payload".into(), event.payload.clone());
    args.insert("mode".into(), Json::String(event.kind.as_str().to_owned()));
    args.insert("trigger".into(), Json::String(trigger.to_owned()));
    // v1 code reads two things off `req`: who is asking, and what they posted.
    // Anything else it reaches for is an Express request, which does not exist
    // here — and an object that answered those too would be pretending.
    args.insert(
        "req".into(),
        json!({
            "user": event.user.clone().unwrap_or(Json::Null),
            "body": event.payload.clone(),
        }),
    );
    Json::Object(args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sc_action::EventKind;

    #[test]
    fn the_v1_argument_object_carries_the_names_a_plugin_destructures() {
        let mut event = Event::new(EventKind::Insert);
        event.channel = Some("books".into());
        event.row = Some(json!({ "id": 1, "title": "Dune" }));
        event.user = Some(json!({ "id": 7, "email": "a@b.c" }));
        let mut config = Attrs::new();
        config.insert("channel".into(), json!("readings"));

        let args = v1_arguments(&event, &config, "publish a reading");
        assert_eq!(args["row"]["title"], json!("Dune"));
        assert_eq!(args["table"]["name"], json!("books"));
        assert_eq!(args["channel"], json!("books"));
        assert_eq!(args["configuration"]["channel"], json!("readings"));
        assert_eq!(args["user"]["email"], json!("a@b.c"));
        assert_eq!(args["mode"], json!("insert"));
        assert_eq!(args["trigger"], json!("publish a reading"));
        assert_eq!(args["req"]["user"]["id"], json!(7));
    }

    #[test]
    fn an_event_with_no_row_and_no_caller_still_has_every_key() {
        let event = Event::new(EventKind::None);
        let args = v1_arguments(&event, &Attrs::new(), "run me");
        for key in [
            "row", "old_row", "table", "channel", "user", "payload", "req",
        ] {
            assert!(args.get(key).is_some(), "missing {key}");
        }
        assert_eq!(args["row"], Json::Null);
        assert_eq!(args["table"], Json::Null);
    }

    #[test]
    fn an_action_with_no_description_is_attributed_to_its_module() {
        let host = Arc::new(ModuleHost::new("/tmp/does-not-need-to-exist"));
        let action = ModuleAction::new("@saltcorn/mqtt", "mqtt_publish", "", Vec::new(), host);
        assert_eq!(action.name(), "mqtt_publish");
        assert!(action.description().contains("@saltcorn/mqtt"));
        assert_eq!(action.module(), "@saltcorn/mqtt");
    }
}
