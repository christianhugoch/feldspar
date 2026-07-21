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

use sc_api::{ApiProvider, EndpointSet, REST_PROVIDER, RestProvider, generate_client};
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
    validate_api_mounts(app)?;
    let tables = app_tables(app, cat)?;
    app.apis
        .iter()
        .map(|api| match api.provider.as_str() {
            REST_PROVIDER => {
                Ok(Box::new(RestProvider::project(&api.mount, &tables)) as Box<dyn ApiProvider>)
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
    let mut set = EndpointSet::new();
    for provider in app_providers(app, cat)? {
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
    Ok(generate_client(&app_endpoints(app, cat)?))
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
