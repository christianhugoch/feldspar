//! Resolving an [`Application`] into its API providers, endpoint set, and typed
//! client (design §13.2/§13.4/§13.1).
//!
//! An [`Application`] is pure data: it names the tables it may touch and the API
//! providers it enables, each on a sub-path. This module is the wiring that turns
//! that declaration into running machinery — resolving the declared tables
//! against the [`Catalog`], building one [`ApiProvider`] per [`ApiConfig`], and
//! collecting their projections into the app's single [`EndpointSet`].
//!
//! That endpoint set is what the app's **TypeScript client** is generated from
//! ([`app_client`]), by the same [`generate_client`] the admin SPA uses. The
//! admin's client is a checked-in artifact with a drift test, because its
//! endpoints are compile-time constants; an app's endpoints depend on which
//! tables it declares, so its client cannot be committed — it is emitted into the
//! app's source tree at build time (see [`crate::build::emit_client`]).

use std::sync::Arc;

use sc_action::{Trigger, TriggerDispatcher};
use sc_api::{ApiProvider, EndpointSet, REST_PROVIDER, RestProvider, generate_client, op_name};
use sc_catalog::{Catalog, Table};
use sc_error::{Error, Result};

use crate::application::Application;
use crate::framework::framework_serves_ui;

/// Check that no API provider claims a path the app's UI needs (§13.2/§13.4).
///
/// A provider mounted at `/` claims **every** path — that is what a root mount
/// means — so an app whose framework serves a UI would answer `GET /` from its
/// API, which has no endpoint there, and the app would be a 404 in a browser
/// with nothing to say why. That is a configuration error, and this is where it
/// is named.
///
/// It is refused only when there is a UI to lose: an app whose framework
/// declares [`serves_ui`](crate::Framework::serves_ui) `false` is an API-only
/// app, and `/` is exactly the right mount for it.
pub fn validate_api_mounts(app: &Application) -> Result<()> {
    check_api_mounts(app, framework_serves_ui(&app.framework.name))
}

/// [`validate_api_mounts`] with the framework's answer supplied — the whole rule,
/// with the registry lookup lifted out so it is one decision on one input.
fn check_api_mounts(app: &Application, serves_ui: bool) -> Result<()> {
    if !serves_ui {
        return Ok(());
    }
    for api in &app.apis {
        if api.mount == "/" {
            return Err(Error::invalid(format!(
                "application `{}` mounts its `{}` API at `/`, which claims every path \
                 and would leave the `{}` framework's UI unreachable; mount the API on \
                 a sub-path such as `/api`",
                app.name, api.provider, app.framework.name
            )));
        }
    }
    Ok(())
}

/// The tables an application declares, resolved against the catalog.
///
/// An app sees only its declared subset (§13.2), so this is the *whole* of the
/// data any of its providers can reach. A declared table that is not in the
/// catalog is an error rather than a silent omission: it means the app is
/// misconfigured, and quietly serving a smaller API would hide that.
pub fn app_tables(app: &Application, cat: &Catalog) -> Result<Vec<Table>> {
    app.tables.iter().map(|id| cat.require(&id.0)).collect()
}

/// The triggers an application exposes, resolved against the live trigger set
/// (§10.2).
///
/// An app's exposed triggers are a **declared subset**, exactly like its tables,
/// and this resolves it the same way [`app_tables`] does — a declared trigger
/// that is not in the live set is an error rather than a silently missing
/// endpoint. The two ways it can be missing say different things and the live
/// set's own `require` distinguishes them: never defined (a typo, or a trigger
/// somebody deleted) versus defined but not usable (its table is gone, its action
/// came from an uninstalled plugin) — the second carries the reason, which is the
/// part the admin needs.
///
/// `dispatcher` is `None` in the contexts that only wanted endpoint shapes.
/// That is fine for an app with no exposed triggers and a **configuration error**
/// for one that has them: silently projecting an app without the endpoints its
/// own generated client calls is the failure this refuses to have.
pub fn app_triggers(
    app: &Application,
    dispatcher: Option<&Arc<TriggerDispatcher>>,
) -> Result<Vec<Trigger>> {
    if app.triggers.is_empty() {
        return Ok(Vec::new());
    }
    let Some(dispatcher) = dispatcher else {
        return Err(Error::config(format!(
            "application `{}` exposes trigger `{}`, but no trigger set is \
             available in this context",
            app.name, app.triggers[0]
        )));
    };
    let live = dispatcher.triggers()?;
    let mut resolved: Vec<Trigger> = Vec::with_capacity(app.triggers.len());
    for declared in &app.triggers {
        let trigger = live.require(&declared.0).map_err(|e| {
            Error::config(format!(
                "application `{}` exposes trigger `{declared}`: {e}",
                app.name
            ))
        })?;
        // Two triggers whose names differ only in punctuation would project one
        // endpoint name (`send_digest` and `sendDigest` are both `runSendDigest`)
        // and the second registration would be a panic in a running server. It is
        // a configuration error, so it is named here rather than survived.
        if let Some(clash) = resolved
            .iter()
            .find(|t| op_name("run", &t.name) == op_name("run", &trigger.name))
        {
            return Err(Error::config(format!(
                "application `{}` exposes both `{}` and `{declared}`, which would \
                 project the same endpoint `{}`; expose one of them",
                app.name,
                clash.name,
                op_name("run", &trigger.name)
            )));
        }
        resolved.push(trigger.clone());
    }
    Ok(resolved)
}

/// Build the API providers an application enables, each on its own sub-path
/// (design §13.4).
///
/// The MVP registers one provider name, `rest`; an unknown name is a
/// configuration error rather than a silently skipped API.
///
/// The mounts are checked first ([`validate_api_mounts`]), so an app saved before
/// that check existed fails to build and to mount — with the reason — rather than
/// coming up as an app that answers every request with a 404 from its API.
pub fn app_providers(app: &Application, cat: &Catalog) -> Result<Vec<Box<dyn ApiProvider>>> {
    app_providers_with(app, cat, None, None)
}

/// [`app_providers`] with the server's JavaScript evaluator and trigger
/// dispatcher injected — what a *running* mount uses, so ownership formulas'
/// reified path (§7.3) has an engine and an exposed trigger has something to run
/// on.
///
/// Both are optional because everything that only needs the endpoint *shapes* —
/// client generation, endpoint collection, tests — has neither and needs neither.
/// A provider without an evaluator fails closed if a formula actually requires
/// one; an app that declares triggers without a dispatcher is refused outright
/// ([`app_triggers`]), because there its absence changes the app's API surface
/// rather than one request's answer.
pub fn app_providers_with(
    app: &Application,
    cat: &Catalog,
    evaluator: Option<std::sync::Arc<dyn sc_expr::JsEvaluator>>,
    dispatcher: Option<&Arc<TriggerDispatcher>>,
) -> Result<Vec<Box<dyn ApiProvider>>> {
    validate_api_mounts(app)?;
    let tables = app_tables(app, cat)?;
    let triggers = app_triggers(app, dispatcher)?;
    app.apis
        .iter()
        .map(|api| match api.provider.as_str() {
            REST_PROVIDER => {
                let mut provider = RestProvider::project_with(&api.mount, &tables, &triggers);
                if let Some(evaluator) = &evaluator {
                    provider = provider.with_evaluator(evaluator.clone());
                }
                if let Some(dispatcher) = dispatcher {
                    provider = provider.with_dispatcher(Arc::clone(dispatcher));
                }
                Ok(Box::new(provider) as Box<dyn ApiProvider>)
            }
            other => Err(Error::config(format!(
                "application `{}` enables unknown API provider `{other}`; \
                 the MVP ships only `{REST_PROVIDER}`",
                app.name
            ))),
        })
        .collect()
}

/// The application's whole API surface: every enabled provider's projection,
/// collected into one [`EndpointSet`].
///
/// Two providers may not contribute the same endpoint name — the name keys both
/// the generated client's methods and server dispatch, so a collision is a
/// configuration error (e.g. two REST providers mounted over the same tables).
pub fn app_endpoints(app: &Application, cat: &Catalog) -> Result<EndpointSet> {
    app_endpoints_with(app, cat, None)
}

/// [`app_endpoints`] resolving the app's exposed triggers against the live set,
/// so the endpoints include what an app's client will be generated to call. The
/// build path passes the server's dispatcher; a caller that has none is only
/// correct for an app that exposes no triggers.
pub fn app_endpoints_with(
    app: &Application,
    cat: &Catalog,
    dispatcher: Option<&Arc<TriggerDispatcher>>,
) -> Result<EndpointSet> {
    let mut set = EndpointSet::new();
    for provider in app_providers_with(app, cat, None, dispatcher)? {
        for endpoint in provider.endpoints().iter() {
            if set.find(&endpoint.name).is_some() {
                return Err(Error::config(format!(
                    "application `{}` has two API endpoints named `{}`; \
                     each provider must project distinct operation names",
                    app.name, endpoint.name
                )));
            }
            set.register(endpoint.clone());
        }
    }
    Ok(set)
}

/// Generate the application's TypeScript API-consumer client from its endpoint
/// set — the same [`generate_client`] machinery the admin API uses (§13.1).
pub fn app_client(app: &Application, cat: &Catalog) -> Result<String> {
    app_client_with(app, cat, None)
}

/// [`app_client`] over [`app_endpoints_with`] — the form the build path uses, so
/// an app that exposes a trigger gets a typed `runFoo(body)` in its client.
pub fn app_client_with(
    app: &Application,
    cat: &Catalog,
    dispatcher: Option<&Arc<TriggerDispatcher>>,
) -> Result<String> {
    Ok(generate_client(&app_endpoints_with(app, cat, dispatcher)?))
}

#[cfg(test)]
mod mount_tests {
    use super::*;
    use crate::application::{ApiConfig, FrameworkRef};
    use crate::framework::{CODE_FRAMEWORK, framework_serves_ui};
    use crate::react::REACT_FRAMEWORK;

    fn app_with_mount(framework: &str, mount: &str) -> Application {
        Application::new("myapp", "myapp", FrameworkRef::new(framework))
            .with_api(ApiConfig::new(sc_api::REST_PROVIDER, mount))
    }

    #[test]
    fn an_api_at_the_root_is_refused_when_the_framework_serves_a_ui() {
        // The failure this prevents: the provider claims `/`, so the app's own
        // pages are answered by an API that has no endpoint there.
        let err = validate_api_mounts(&app_with_mount(REACT_FRAMEWORK, "/")).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("claims every path"), "{msg}");
        assert!(msg.contains("/api"), "{msg}");
    }

    #[test]
    fn a_sub_path_mount_is_accepted() {
        validate_api_mounts(&app_with_mount(REACT_FRAMEWORK, "/api")).unwrap();
        validate_api_mounts(&app_with_mount(CODE_FRAMEWORK, "/v1/api")).unwrap();
    }

    #[test]
    fn an_app_whose_framework_serves_no_ui_may_claim_the_root() {
        // The rule is the framework's to answer: with no UI to lose, `/` is the
        // right mount for an API-only app.
        check_api_mounts(&app_with_mount("headless", "/"), false).unwrap();
    }

    #[test]
    fn every_registered_framework_serves_a_ui_and_an_unknown_one_is_assumed_to() {
        for info in crate::framework::registered_framework_info() {
            assert!(info.serves_ui, "{} should serve a UI", info.name);
            assert!(framework_serves_ui(&info.name));
        }
        // Unknown: assume there is a UI to protect — the safe direction.
        assert!(framework_serves_ui("something-else"));
    }
}
