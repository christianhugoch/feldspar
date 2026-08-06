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
use sc_api::{
    ApiProvider, EndpointSet, GRAPHQL_PROVIDER, GraphqlProvider, REST_PROVIDER, RestProvider,
    generate_client, op_name,
};
use sc_catalog::{Catalog, Table};
use sc_error::{Error, Result};

use crate::application::Application;
use crate::framework::framework_serves_ui;

/// How an API provider presents itself to an admin enabling one: a human name
/// and a sentence saying what protocol they get.
///
/// The sibling of [`FrameworkInfo`](crate::FrameworkInfo), and it exists for the
/// same reason: the admin's application form offers the registered names as a
/// **list** rather than a free-text box, so `graphql` is discoverable and a typo
/// is refused at the keyboard rather than surfacing later as a mount failure on
/// a saved application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiProviderInfo {
    /// The registry key, as stored in an [`ApiConfig`](crate::ApiConfig).
    pub name: String,
    /// A human-facing name.
    pub label: String,
    /// One sentence: what this provider serves, and what a caller does with it.
    pub description: String,
    /// The sub-path this provider is usually mounted at — what the form fills in
    /// when the admin picks it.
    pub default_mount: String,
}

/// Every registered API provider with its presentation, in the order an admin
/// should be offered them.
///
/// **This is the list [`app_providers_with`] switches on**, so a provider that
/// is offered is a provider that mounts: the two cannot drift, because the
/// unknown-provider error is written from this list.
pub fn registered_api_provider_info() -> Vec<ApiProviderInfo> {
    vec![
        ApiProviderInfo {
            name: REST_PROVIDER.to_owned(),
            label: "REST".to_owned(),
            description: "A route per operation over the app's tables and exposed \
                          triggers, with a typed TypeScript client generated from it. \
                          The one to take unless you know you want the other."
                .to_owned(),
            default_mount: "/api".to_owned(),
        },
        ApiProviderInfo {
            name: GRAPHQL_PROVIDER.to_owned(),
            label: "GraphQL".to_owned(),
            description: "One endpoint the caller writes the shape of: nested \
                          relations and constrained child aggregates in one round \
                          trip, with the SDL served beside it. Sits alongside REST \
                          rather than replacing it."
                .to_owned(),
            default_mount: sc_api::GRAPHQL_DEFAULT_MOUNT.to_owned(),
        },
    ]
}

/// The registered provider names, comma-separated — what an error naming what is
/// available says.
fn registered_provider_names() -> String {
    registered_api_provider_info()
        .iter()
        .map(|p| format!("`{}`", p.name))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Check that no API provider claims a path the app's UI needs, and that no two
/// claim the same one (§13.2/§13.4).
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
///
/// Two providers on one mount is refused whatever the framework serves, because
/// there is no arrangement in which it is what the admin meant: the router
/// resolves a request to **one** provider by longest matching mount, so the
/// loser of the tie is a whole API that is mounted, generated a client for, and
/// unreachable. REST and GraphQL coexisting on one application is the point of
/// this milestone — on `/api` and `/graphql`, not twice on `/api`.
pub fn validate_api_mounts(app: &Application) -> Result<()> {
    check_api_mounts(app, framework_serves_ui(&app.framework.name))
}

/// [`validate_api_mounts`] with the framework's answer supplied — the whole rule,
/// with the registry lookup lifted out so it is one decision on one input.
fn check_api_mounts(app: &Application, serves_ui: bool) -> Result<()> {
    for (i, api) in app.apis.iter().enumerate() {
        if let Some(other) = app.apis[..i].iter().find(|a| a.mount == api.mount) {
            return Err(Error::invalid(format!(
                "application `{}` mounts both its `{}` and `{}` APIs at `{}`; a request \
                 resolves to one provider, so the other would be unreachable — give \
                 them separate sub-paths",
                app.name, other.provider, api.provider, api.mount
            )));
        }
        if serves_ui && api.mount == "/" {
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
/// [`registered_api_provider_info`] is the list of names this understands; an
/// unknown one is a configuration error rather than a silently skipped API.
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
            GRAPHQL_PROVIDER => {
                let mut provider = graphql_provider(app, &api.mount, &tables)?;
                if let Some(evaluator) = &evaluator {
                    provider = provider.with_evaluator(evaluator.clone());
                }
                Ok(Box::new(provider) as Box<dyn ApiProvider>)
            }
            other => Err(Error::config(format!(
                "application `{}` enables unknown API provider `{other}`; \
                 this server registers {}",
                app.name,
                registered_provider_names()
            ))),
        })
        .collect()
}

/// Project `app`'s GraphQL API at `mount` over `tables` — the one place a
/// [`GraphqlProvider`] is built for an application.
///
/// Shared by the mount path ([`app_providers_with`]) and the build path
/// ([`app_graphql_sdl`]) so the SDL a build writes into the app's source tree is
/// the SDL its running mount answers introspection with. Two constructions with
/// the same arguments would be the same schema *by coincidence*; one is the same
/// schema because it is the same call.
///
/// The tables are the app's declared subset, already resolved — the same value
/// the REST provider is projected from, so one application cannot expose two
/// different table subsets through its two APIs.
fn graphql_provider(app: &Application, mount: &str, tables: &[Table]) -> Result<GraphqlProvider> {
    // A schema that will not build is a mount failure naming the table that
    // caused it: an application whose API is half described is worse than one
    // that refuses to come up, because the half nobody notices is the wrong one.
    let provider = GraphqlProvider::project(mount, tables).map_err(|e| {
        Error::config(format!(
            "application `{}` cannot project its GraphQL API: {e}",
            app.name
        ))
    })?;
    // A `File` field answers with its path and the URL the *REST* provider
    // serves the bytes at, so a GraphQL field never becomes a second download
    // path. An application with no REST provider has no such URL to give; the
    // provider's default mount is what the field then names, which is at least
    // honest about where the bytes would be served from.
    Ok(
        match app.apis.iter().find(|a| a.provider == REST_PROVIDER) {
            Some(rest) => provider.with_file_mount(&rest.mount),
            None => provider,
        },
    )
}

/// An application's GraphQL API as its **build** needs to know it: where it is
/// mounted, and the SDL of the schema served there.
///
/// Both, together, because the two generated files need one each and they have
/// to describe the same API: the client posts to `mount`, and `schema.graphql`
/// is what `gql.tada` type-checks the documents it posts against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppGraphql {
    /// The sub-path the provider is mounted at within the application.
    pub mount: String,
    /// The schema's SDL — `Schema::sdl()` of the projection that will be mounted.
    pub sdl: String,
}

/// The application's GraphQL projection, or `None` for an application that does
/// not enable the provider.
///
/// This is what the build writes into the app's source tree beside the generated
/// REST client. The SDL is the file `gql.tada` type-checks the app's own queries
/// against, so a query the schema no longer answers is a **build failure** in
/// the app's own `tsc --noEmit` rather than an error at run time.
pub fn app_graphql(app: &Application, cat: &Catalog) -> Result<Option<AppGraphql>> {
    let Some(api) = app.apis.iter().find(|a| a.provider == GRAPHQL_PROVIDER) else {
        return Ok(None);
    };
    let tables = app_tables(app, cat)?;
    let provider = graphql_provider(app, &api.mount, &tables)?;
    Ok(Some(AppGraphql {
        mount: provider.mount(),
        sdl: provider.sdl(),
    }))
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

    /// REST at `/api` and GraphQL at `/graphql` on one application — the
    /// arrangement this milestone exists to make possible, and the only mount
    /// check it has to pass.
    #[test]
    fn rest_and_graphql_coexist_on_separate_sub_paths() {
        let app = Application::new("myapp", "myapp", FrameworkRef::new(REACT_FRAMEWORK))
            .with_api(ApiConfig::new(REST_PROVIDER, "/api"))
            .with_api(ApiConfig::new(GRAPHQL_PROVIDER, "/graphql"));
        validate_api_mounts(&app).unwrap();
    }

    #[test]
    fn two_providers_on_one_mount_are_refused_naming_both() {
        // Not a stylistic objection: the router resolves a path to one provider,
        // so the loser is a whole API that is mounted and unreachable.
        let app = Application::new("myapp", "myapp", FrameworkRef::new(REACT_FRAMEWORK))
            .with_api(ApiConfig::new(REST_PROVIDER, "/api"))
            .with_api(ApiConfig::new(GRAPHQL_PROVIDER, "/api"));
        let msg = validate_api_mounts(&app).unwrap_err().to_string();
        assert!(msg.contains(REST_PROVIDER), "{msg}");
        assert!(msg.contains(GRAPHQL_PROVIDER), "{msg}");
        assert!(msg.contains("/api"), "{msg}");
    }

    #[test]
    fn a_colliding_mount_is_refused_even_for_an_api_only_app() {
        // The `/` rule is the framework's to waive; this one is not — an
        // unreachable API is unreachable whatever the framework serves.
        let app = Application::new("myapp", "myapp", FrameworkRef::new("headless"))
            .with_api(ApiConfig::new(REST_PROVIDER, "/"))
            .with_api(ApiConfig::new(GRAPHQL_PROVIDER, "/"));
        assert!(check_api_mounts(&app, false).is_err());
    }

    /// Every offered provider is a provider that mounts. The list drives the
    /// admin's select, so a name in it that `app_providers_with` does not know
    /// would be a configuration error an admin was *invited* to make.
    #[test]
    fn every_offered_provider_name_is_one_the_mount_path_switches_on() {
        let names: Vec<String> = registered_api_provider_info()
            .into_iter()
            .map(|p| p.name)
            .collect();
        assert!(names.contains(&REST_PROVIDER.to_owned()), "{names:?}");
        assert!(names.contains(&GRAPHQL_PROVIDER.to_owned()), "{names:?}");
        for info in registered_api_provider_info() {
            assert!(
                matches!(info.name.as_str(), REST_PROVIDER | GRAPHQL_PROVIDER),
                "`{}` is offered but `app_providers_with` does not build it",
                info.name
            );
            assert!(info.default_mount.starts_with('/'), "{info:?}");
            assert!(
                !info.label.is_empty() && !info.description.is_empty(),
                "{info:?}"
            );
        }
        // And the error an unknown name gets names what is available.
        let listed = registered_provider_names();
        assert!(
            listed.contains(REST_PROVIDER) && listed.contains(GRAPHQL_PROVIDER),
            "{listed}"
        );
    }
}
