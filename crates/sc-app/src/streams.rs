//! The streams an application exposes for observation (TODO "Streams" §10).
//!
//! An app exposes streams the way it exposes triggers: a declared subset of
//! names ([`StreamRef`](crate::StreamRef)), on the same principle — a stream is
//! server-side configuration, and it becomes reachable from outside only
//! because an app said so. This module is the three things that follow from
//! that:
//!
//! 1. **Where the socket is.** [`stream_socket_path`] is the one place
//!    `{mount}/streams/{name}/observe` is spelled, and [`stream_in_path`] reads
//!    a request's path back against it. The generated client and the server's
//!    router both go through them, so the two cannot disagree about a path.
//! 2. **What an element looks like**, in the vocabulary the client generator
//!    speaks: [`element_value_schema`] turns an [`ElementType`] into a
//!    [`TypeSchema`], which is what gives an app's subscription a typed
//!    callback instead of an `unknown`.
//! 3. **Which streams they are**, resolved: [`app_streams`] loads each declared
//!    name from `_fd_streams` and computes its element type from its provider,
//!    refusing a name that does not resolve exactly as
//!    [`app_triggers`](crate::app_triggers) does — an app naming a stream that
//!    is not there is misconfigured, and quietly generating a client without
//!    the method its own code calls would hide that.
//!
//! ## Why the provider registry is installed rather than passed
//!
//! An element type is a function of the provider *and* the configuration (§3),
//! so resolving one needs the [`StreamRegistry`] — which is the running
//! server's, since a module can add to it. Every function that emits a client
//! would otherwise grow a second optional service parameter beside the trigger
//! dispatcher, down through the build path and every test that calls it. So the
//! registry is **installed**, the way a [`FrameworkFactory`](crate::FrameworkFactory)
//! is: `sc-server` puts its own in at boot and on every module change, and a
//! process that has not (a test, a `feldspar` command that is not serving)
//! falls back to the compiled-in providers, which is exactly what such a
//! process has.

use std::sync::{Arc, RwLock};

use sc_api::{StreamExport, StructField, TypeSchema, ValueType};
use sc_catalog::Catalog;
use sc_error::{Error, Result};
use sc_stream::{ElementType, Stream, StreamRegistry};
use sc_types::BasicType;

use crate::application::Application;

/// The path segment that separates an app's mount from the stream's name.
const STREAMS_SEGMENT: &str = "streams";
/// The last segment of an observe socket's path.
const OBSERVE_SEGMENT: &str = "observe";

/// The provider registry this process types an app's streams against.
///
/// `None` until a server installs one; see the module docs for why it is
/// installed rather than threaded through the build path.
static REGISTRY: RwLock<Option<Arc<StreamRegistry>>> = RwLock::new(None);

/// Install the registry an app's exposed streams are resolved against,
/// replacing any previous one.
///
/// Called by `sc-server` at boot and after a module change, for the same reason
/// the supervisor's own registry is replaced then: a module can supply a stream
/// provider, and a client generated a minute later must type its elements from
/// the provider that is actually installed.
pub fn install_stream_registry(registry: Arc<StreamRegistry>) {
    let mut guard = match REGISTRY.write() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    *guard = Some(registry);
}

/// The installed registry, or the compiled-in providers when nothing has
/// installed one.
pub fn stream_registry() -> Arc<StreamRegistry> {
    let installed = match REGISTRY.read() {
        Ok(guard) => guard.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    };
    installed.unwrap_or_else(|| {
        // A registry built from the compiled-in providers cannot have a
        // duplicate name in it — the duplicate is the only thing that fails —
        // so an empty one is the honest fallback rather than a panic in a
        // process that was only generating a client.
        Arc::new(sc_stream::builtin_registry().unwrap_or_default())
    })
}

/// One stream an application exposes, resolved: its row and what its elements
/// are.
#[derive(Debug, Clone)]
pub struct ExposedStream {
    /// The stored stream — its name, its `min_role`, whether it is enabled.
    pub stream: Stream,
    /// What an element of it is, from its provider and its configuration (§3).
    pub element_type: ElementType,
}

/// The socket path for `name` on `app`: `{mount}/streams/{name}/observe`.
///
/// The mount is the app's **first API mount**, because that is what "beside the
/// endpoint set" means on the wire: an app whose data is at `/api` observes its
/// streams at `/api/streams/…`, and its generated client has one base URL for
/// both. An app with no API provider has no mount to sit beside, and its
/// sockets are at the root of its subdomain.
pub fn stream_socket_path(app: &Application, name: &str) -> String {
    let mount = app
        .apis
        .first()
        .map(|api| api.mount.trim_end_matches('/'))
        .unwrap_or("");
    format!("{mount}/{STREAMS_SEGMENT}/{name}/{OBSERVE_SEGMENT}")
}

/// The stream `path` names on `app`, if it is an observe socket's path at all.
///
/// The inverse of [`stream_socket_path`], and the router's half of it: the
/// server matches a request against this rather than against a pattern of its
/// own, so a change to one shape cannot leave a client calling a path nothing
/// serves. It says nothing about whether the app *exposes* that stream — that
/// is [`Application::exposes_stream`], and the distinction matters because the
/// two refusals differ (a 404 for a path that is not a socket, a 404 naming the
/// stream for one the app did not expose).
pub fn stream_in_path<'a>(app: &Application, path: &'a str) -> Option<&'a str> {
    let mount = app
        .apis
        .first()
        .map(|api| api.mount.trim_end_matches('/'))
        .unwrap_or("");
    let rest = path.strip_prefix(mount)?;
    let rest = rest.strip_prefix('/')?;
    let rest = rest.strip_prefix(STREAMS_SEGMENT)?;
    let rest = rest.strip_prefix('/')?;
    let (name, tail) = rest.rsplit_once('/')?;
    (tail == OBSERVE_SEGMENT && !name.is_empty() && !name.contains('/')).then_some(name)
}

/// The shape of an element's `value`, as the client generator's vocabulary
/// (§13.1) states it.
///
/// - `Json` is a record of the declared keys. A key that is not `required` is
///   optional-and-nullable, because §4 says a declared key that is absent is
///   `null`.
/// - `Text` is a string. The encoding is settled at save time (UTF-8 or
///   refused), so there is nothing left for a type to say about it.
/// - `Binary` is [`ValueType::Bytes`], which is base64 in a string — the same
///   thing a bytes column is on the wire, spelled the same way.
pub fn element_value_schema(element_type: &ElementType) -> TypeSchema {
    match element_type {
        ElementType::Json { keys } => TypeSchema::struct_of(keys.iter().map(|key| {
            let value = TypeSchema::value(value_type_of(&key.r#type));
            StructField::new(
                key.name.clone(),
                if key.required {
                    value
                } else {
                    TypeSchema::optional(value)
                },
            )
        })),
        ElementType::Text { .. } => TypeSchema::text(),
        ElementType::Binary => TypeSchema::value(ValueType::Bytes),
    }
}

/// A declared key's basic type as a wire type. `ValueType::from_basic` is the
/// mapping; this exists only so the call site above reads as one line.
fn value_type_of(basic: &BasicType) -> ValueType {
    ValueType::from_basic(basic)
}

/// The streams `app` exposes, resolved against the stored rows and the
/// installed provider registry.
///
/// A declared stream that does not exist, or whose provider is not installed,
/// or whose configuration no longer yields an element type, is an
/// [`Error::config`] naming the app and the stream — [`app_triggers`](crate::app_triggers)'
/// refusal, for its reason: the app's own generated client calls a method that
/// would not be there, and a smaller client is a worse answer than a named
/// failure.
///
/// A **disabled** stream resolves: it is configuration that exists, its socket
/// is mounted, and what an observer gets is the honest "not running on this
/// server". Enabling it must not require rebuilding the app.
pub async fn app_streams(app: &Application, cat: &Catalog) -> Result<Vec<ExposedStream>> {
    if app.streams.is_empty() {
        return Ok(Vec::new());
    }
    let registry = stream_registry();
    let mut resolved = Vec::with_capacity(app.streams.len());
    for declared in &app.streams {
        let stream = sc_stream::load_stream_by_name(cat, &declared.0)
            .await?
            .ok_or_else(|| {
                Error::config(format!(
                    "application `{}` exposes stream `{declared}`, which no longer exists",
                    app.name
                ))
            })?;
        let provider = registry.require(&stream.provider).map_err(|e| {
            Error::config(format!(
                "application `{}` exposes stream `{declared}`: {e}",
                app.name
            ))
        })?;
        // Resolved rather than read: an application's client is generated
        // against the element type, and a module's provider only knows it by
        // asking the worker (TODO "Streams" §12).
        let element_type = provider
            .resolve_element_type(&stream.configuration)
            .await
            .map_err(|e| {
                Error::config(format!(
                    "application `{}` exposes stream `{declared}`: {e}",
                    app.name
                ))
            })?;
        resolved.push(ExposedStream {
            stream,
            element_type,
        });
    }
    Ok(resolved)
}

/// The resolved streams as the client generator takes them.
pub fn stream_exports(app: &Application, streams: &[ExposedStream]) -> Vec<StreamExport> {
    streams
        .iter()
        .map(|exposed| StreamExport {
            name: exposed.stream.name.clone(),
            path: stream_socket_path(app, &exposed.stream.name),
            value: element_value_schema(&exposed.element_type),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::{ApiConfig, FrameworkRef, StreamRef};
    use sc_stream::ElementField;

    fn app() -> Application {
        Application::new("Blog", "blog", FrameworkRef::new("code"))
            .with_api(ApiConfig::new("rest", "/api"))
            .with_stream(StreamRef::new("boiler"))
    }

    #[test]
    fn the_socket_sits_beside_the_endpoint_set() {
        assert_eq!(
            stream_socket_path(&app(), "boiler"),
            "/api/streams/boiler/observe"
        );
        // An app with no API provider has no mount to sit beside.
        let bare = Application::new("Bare", "bare", FrameworkRef::new("code"));
        assert_eq!(
            stream_socket_path(&bare, "boiler"),
            "/streams/boiler/observe"
        );
    }

    #[test]
    fn a_path_is_read_back_against_the_shape_that_wrote_it() {
        let app = app();
        let path = stream_socket_path(&app, "boiler");
        assert_eq!(stream_in_path(&app, &path), Some("boiler"));
        // Not an observe socket at all.
        assert_eq!(stream_in_path(&app, "/api/posts"), None);
        assert_eq!(stream_in_path(&app, "/api/streams/boiler"), None);
        assert_eq!(stream_in_path(&app, "/streams/boiler/observe"), None);
        assert_eq!(stream_in_path(&app, "/api/streams//observe"), None);
        // A name the app does not expose still *parses* — whether it is exposed
        // is a separate question with a separate answer.
        assert_eq!(
            stream_in_path(&app, "/api/streams/meter/observe"),
            Some("meter")
        );
    }

    #[test]
    fn an_element_type_becomes_the_shape_of_value() {
        let json = element_value_schema(&ElementType::json([
            ElementField::new("temperature", BasicType::Float).required(),
            ElementField::new("label", BasicType::Text),
        ]));
        assert_eq!(
            json,
            TypeSchema::struct_of([
                StructField::new("temperature", TypeSchema::value(ValueType::Float)),
                // §4: a declared key that is absent is null.
                StructField::new(
                    "label",
                    TypeSchema::optional(TypeSchema::value(ValueType::Text))
                ),
            ])
        );
        assert_eq!(
            element_value_schema(&ElementType::text()),
            TypeSchema::text()
        );
        assert_eq!(
            element_value_schema(&ElementType::Binary),
            TypeSchema::value(ValueType::Bytes)
        );
    }
}
