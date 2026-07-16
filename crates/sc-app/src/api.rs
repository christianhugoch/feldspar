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
pub fn app_providers(app: &Application, cat: &Catalog) -> Result<Vec<Box<dyn ApiProvider>>> {
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
