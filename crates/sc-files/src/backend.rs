//! The file-store **backend registry**: which backends exist, what settings each
//! declares, and how a [`FileStoreDef`] becomes a connected [`FileStore`].
//!
//! This is the same "settings as data" move that makes "the admin picks a
//! framework" work (design §13.3), applied to stores. The admin UI must render a
//! settings form for a backend it knows nothing about — including one supplied
//! later by a guest language through `sc-code` — with no per-backend special
//! case. So a backend declares its settings as [`FormField`]s, the one
//! vocabulary §6.2 gives every configurable extension point, and the UI renders
//! whatever it is handed.
//!
//! The MVP registers exactly one backend, [`LOCAL_BACKEND`]. The registry is not
//! ceremony for that one case: it is what makes adding S3 or a git remote a
//! change *here* rather than a change in the admin UI.
//!
//! ## Two kinds of wrong, kept apart
//!
//! A store's configuration can be wrong in two quite different ways, and
//! conflating them would make the admin UI unusable:
//!
//! - **Structurally wrong** — a missing `path`, a `path` that is a number, a
//!   setting the backend has never heard of, an unknown backend. This is the
//!   admin's typo, it is knowable without touching the filesystem, and it is
//!   always the same answer on every machine. [`validate_file_store_config`]
//!   catches it, and `save_file_store` refuses the save, so the mistake is
//!   caught while the admin is still looking at the form.
//! - **Currently unreachable** — a perfectly well-formed definition whose
//!   directory has been unmounted, renamed, or not created yet. This is not a
//!   typo and often not the admin's fault; it can become true *after* a
//!   successful save, and it can stop being true without anything being edited.
//!   [`connect_from_def`] reports it.
//!
//! Only the first blocks a save. That is deliberate and follows from §1.1's
//! model: a definition is inert data that may describe a store which does not
//! currently work, and such a store must stay listable and **editable** — an
//! admin whose disk was unmounted needs to be able to open that store and fix
//! its path, which they could not do if saving demanded a reachable directory.
//! So reachability is reported, never enforced at save time.

use std::sync::Arc;

use sc_error::{Context, Error, Repr, Result};
use sc_types::{Attrs, BasicType, FormField, Operation, validate_attrs};

use crate::def::{
    CFG_BRANCH, CFG_CREATE, CFG_KEY_PATH, CFG_PATH, CFG_PUBLIC_KEY, CFG_URL, FileStoreDef,
    GIT_BACKEND, LOCAL_BACKEND,
};
use crate::git::{GitFileStore, GitRepo, git_operations};
use crate::local::LocalFileStore;
use crate::store::FileStore;

/// The settings the [`local`](LOCAL_BACKEND) backend needs: which directory, and
/// whether to create it.
///
/// A free function as well as something the registry hands out, mirroring
/// `code_config_spec` in `sc-app`: the admin UI and the save-time check both
/// need a backend's settings *before* there is an instance to ask, and a
/// `LocalFileStore` cannot exist until its directory does — which is precisely
/// the thing being configured.
pub fn local_config_spec() -> Vec<FormField> {
    vec![
        FormField::new(CFG_PATH, BasicType::Text)
            .label("Directory")
            .required(),
        // Defaults to false: creating directories on the server's filesystem is
        // a side effect an admin should opt into, and silently creating one is
        // how a typo'd path becomes a new empty store instead of an error.
        FormField::new(CFG_CREATE, BasicType::Bool)
            .label("Create the directory if it does not exist")
            .default_value(false),
    ]
}

/// The settings the [`git`](GIT_BACKEND) backend needs: which repository, which
/// branch, and how to authenticate.
///
/// Note what is *not* here: where to put the clone. A `local` store's directory
/// is the admin's own and only they can name it; a git clone is a directory
/// Saltcorn creates, so Saltcorn places it (see [`clone_path`](crate::clone_path))
/// and asking would be asking for a decision the admin has no basis to make.
///
/// `key_path` and `public_key` are ordinary settings even though the deploy-key
/// button usually fills them in, because an admin pointing a store at a key that
/// already exists on the machine is a configuration Saltcorn has no business
/// refusing.
pub fn git_config_spec() -> Vec<FormField> {
    vec![
        FormField::new(CFG_URL, BasicType::Text)
            .label("Repository URL")
            .required(),
        FormField::new(CFG_BRANCH, BasicType::Text).label("Branch (blank for the default)"),
        FormField::new(CFG_KEY_PATH, BasicType::Text).label("SSH private key file"),
        FormField::new(CFG_PUBLIC_KEY, BasicType::Text)
            .label("Deploy key (public)")
            // A key is one long line the admin copies out, not a value they
            // type; the hint is what gets it a text area without the UI knowing
            // that this particular setting is a key.
            .multiline(),
    ]
}

/// The operations a backend offers beyond its settings (§6.2's vocabulary for
/// *acts*, [`Operation`]) — what the admin UI renders as buttons.
///
/// The [`local`](LOCAL_BACKEND) backend has none: there is nothing to do to a
/// directory that reading and writing files does not already cover. That is the
/// shape most backends will have, and it is why operations are declared rather
/// than assumed.
pub fn backend_operations(name: &str) -> Result<Vec<Operation>> {
    match name {
        LOCAL_BACKEND => Ok(Vec::new()),
        GIT_BACKEND => Ok(git_operations()),
        other => Err(unknown_backend(other)),
    }
}

/// Run one of a backend's declared [`operations`](backend_operations).
///
/// `def` is **mutable** because an operation may configure as well as act: the
/// deploy-key generator fills in two settings, and a clone records where it
/// cloned to. The caller persists whatever changed — a
/// [`Configure`](OperationScope::Configure) operation's changes go back to the
/// form the admin is still filling in, an [`Instance`](OperationScope::Instance)
/// operation's to the store's row.
///
/// Returns text for the admin: a command's own output, or a summary. Anything
/// that went wrong is an `Err` carrying the same, since the reason a push failed
/// is the actionable part.
pub async fn run_backend_operation(
    def: &mut FileStoreDef,
    operation: &str,
    input: &Attrs,
) -> Result<String> {
    let declared = backend_operations(&def.backend)?;
    let spec = declared
        .iter()
        .find(|op| op.name == operation)
        .ok_or_else(|| {
            Error::invalid(format!(
                "the `{}` backend has no operation `{operation}`{}",
                def.backend,
                if declared.is_empty() {
                    String::new()
                } else {
                    format!(
                        "; it offers {}",
                        declared
                            .iter()
                            .map(|op| op.name.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                }
            ))
        })?;
    // The operation's arguments are `FormField`s, so they are checked by the
    // same code that checks settings — a missing commit message is caught here,
    // with the same message shape, rather than by each operation separately.
    validate_attrs(&spec.input_spec, input)?;

    match def.backend.as_str() {
        GIT_BACKEND => crate::git::run_git_operation(def, operation, input).await,
        // Unreachable: `backend_operations` above returned a spec, so the
        // backend both exists and declares this operation.
        other => Err(Error::config(format!(
            "file-store backend `{other}` declares operation `{operation}` but cannot run it"
        ))),
    }
}

/// The error for a backend nothing implements, naming what there is.
fn unknown_backend(name: &str) -> Error {
    Error::config(format!(
        "unknown file-store backend `{name}`; the registered backends are {}",
        registered_backends().join(", ")
    ))
}

/// The names of every registered backend — what the admin UI lists so an admin
/// can pick one and be shown its [`backend_config_spec`].
///
/// This is the single place that enumerates them, so a new backend is listed by
/// adding it here (and to [`backend_config_spec`] and [`connect_from_def`]).
pub fn registered_backends() -> Vec<String> {
    vec![LOCAL_BACKEND.to_owned(), GIT_BACKEND.to_owned()]
}

/// The settings the backend registered under `name` declares — the registry
/// lookup, resolving a [`FileStoreDef`]'s backend name to a spec without needing
/// an instance.
///
/// An unknown name is a configuration error rather than a backend with no
/// settings, mirroring `framework_config_spec`: a store whose backend nothing
/// implements cannot be connected, and saying so beats accepting it silently and
/// failing later with no explanation.
pub fn backend_config_spec(name: &str) -> Result<Vec<FormField>> {
    match name {
        LOCAL_BACKEND => Ok(local_config_spec()),
        GIT_BACKEND => Ok(git_config_spec()),
        other => Err(unknown_backend(other)),
    }
}

/// Check a [`FileStoreDef`]'s settings against its backend's declared spec.
///
/// Called **on save** (see `sc_catalog::save_file_store`), which is the point of
/// it: a missing or ill-typed setting is the admin's to fix, and the admin is
/// standing in front of the form. Discovering it at connect time means a store
/// that silently never comes up.
///
/// This is the *structural* half of "is this store configured correctly" — see
/// the module docs for why it deliberately does not touch the filesystem.
pub fn validate_file_store_config(def: &FileStoreDef) -> Result<()> {
    let spec = backend_config_spec(&def.backend)?;
    validate_attrs(&spec, &def.config).map_err(|e| {
        // Name the store and its backend as well as the setting. Rebuilt rather
        // than wrapped, for the reason `validate_framework_config` documents:
        // `Invalid` renders its own "invalid:" prefix, so formatting the whole
        // error into a new one would say it twice, and a `Context` would show
        // only the context and hide the setting — the part the admin needs.
        if let Repr::Invalid(msg) = e.repr() {
            Error::invalid(format!(
                "file store `{}` (backend `{}`): {msg}",
                def.name, def.backend
            ))
        } else {
            e
        }
    })
}

/// Turn a stored definition into a connected [`FileStore`] — the one place a
/// definition becomes an instance.
///
/// This is where **reachability** is decided, and an error here means "this
/// well-formed store is not currently usable", not "this configuration is
/// wrong": the directory may have been unmounted since the definition was saved.
/// Callers surface it rather than discarding the store — a store that failed to
/// connect must still be listed and editable, since editing it is how the admin
/// fixes it. Boot connects every stored store this way, and one failure must not
/// stop the others (TODO §1.3).
///
/// Settings are validated first, so a structurally broken definition gets the
/// clear "you mis-configured this" error rather than a confusing filesystem one.
pub fn connect_from_def(def: &FileStoreDef) -> Result<Arc<dyn FileStore>> {
    validate_file_store_config(def)?;

    match def.backend.as_str() {
        LOCAL_BACKEND => {
            let path = def.setting(CFG_PATH).ok_or_else(|| {
                // Unreachable in practice — `path` is required and validation
                // ran above — but the alternative is an `unwrap`, and a library
                // that cannot panic is worth the four lines (principle 5).
                Error::invalid(format!(
                    "file store `{}` has no `{CFG_PATH}` setting",
                    def.name
                ))
            })?;

            if creates_directory(def) && !std::path::Path::new(path).exists() {
                std::fs::create_dir_all(path).with_context(|| {
                    format!("creating directory {path} for file store `{}`", def.name)
                })?;
            }

            let store = LocalFileStore::new(&def.name, path)
                .with_context(|| format!("connecting file store `{}`", def.name))?;
            Ok(Arc::new(store) as Arc<dyn FileStore>)
        }
        GIT_BACKEND => {
            // Deliberately does **not** clone. Connecting happens at boot and on
            // every save, and both must be fast, offline-safe and free of
            // surprises: a clone is a network operation that can hang, can
            // authenticate as the wrong identity, and — on a store whose remote
            // has been repointed — could write a whole tree nobody asked for.
            // `GitRepo::ensure_cloned` is the explicit, awaited path, and the
            // server calls it when the admin saves the store.
            let repo = GitRepo::from_def(def)?;
            let store = GitFileStore::new(&def.name, repo)
                .with_context(|| format!("connecting file store `{}`", def.name))?;
            Ok(Arc::new(store) as Arc<dyn FileStore>)
        }
        // Unreachable while `validate_file_store_config` runs first, which
        // rejects an unknown backend; kept so adding a backend to the registry
        // without adding it here is a clear error rather than a fallthrough.
        other => Err(Error::config(format!(
            "file-store backend `{other}` declares settings but cannot be connected"
        ))),
    }
}

/// Whether a definition asks for its directory to be created. Absent or
/// non-boolean reads as `false` — the safe direction, and validation has already
/// rejected a non-boolean by the time this is reached.
fn creates_directory(def: &FileStoreDef) -> bool {
    def.config
        .get(CFG_CREATE)
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The error from a failed connect, as a string — the **whole** causal
    /// chain, which is what `connect_file_store_def` records and the admin UI
    /// shows. The outermost layer is only "connecting file store `x`"; the
    /// reason is underneath it.
    ///
    /// `unwrap_err` is unavailable here: it requires the `Ok` type to be `Debug`
    /// and `Arc<dyn FileStore>` is not, a store being a live handle rather than
    /// a value to print.
    fn connect_err(def: &FileStoreDef) -> String {
        match connect_from_def(def) {
            Ok(store) => panic!("expected `{}` not to connect", store.name()),
            Err(e) => sc_error::format_causes(&e),
        }
    }

    /// A unique temp path that does not exist yet.
    fn temp_path(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "sc-files-backend-{tag}-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        ))
    }

    #[test]
    fn the_local_backend_declares_its_settings() {
        let spec = local_config_spec();
        let names: Vec<&str> = spec.iter().map(|f| f.name()).collect();
        assert_eq!(names, [CFG_PATH, CFG_CREATE]);

        // Every setting carries a human label, because the admin UI renders this
        // and nothing else knows what these mean.
        assert!(spec.iter().all(|f| !f.base.label.is_empty()));

        // The directory is the admin's to state; creating it is opt-in, so it
        // defaults to false rather than being required.
        let required: Vec<&str> = spec
            .iter()
            .filter(|f| f.required)
            .map(|f| f.name())
            .collect();
        assert_eq!(required, [CFG_PATH]);
        assert_eq!(
            spec.iter()
                .find(|f| f.name() == CFG_CREATE)
                .unwrap()
                .default,
            Some(serde_json::json!(false))
        );
    }

    #[test]
    fn the_registry_resolves_a_name_to_the_same_spec() {
        assert_eq!(registered_backends(), [LOCAL_BACKEND, GIT_BACKEND]);
        assert_eq!(
            backend_config_spec(LOCAL_BACKEND).unwrap(),
            local_config_spec()
        );
        assert_eq!(backend_config_spec(GIT_BACKEND).unwrap(), git_config_spec());
    }

    #[test]
    fn the_git_backend_asks_for_a_url_and_nothing_else_mandatory() {
        let spec = git_config_spec();
        let names: Vec<&str> = spec.iter().map(|f| f.name()).collect();
        assert_eq!(names, [CFG_URL, CFG_BRANCH, CFG_KEY_PATH, CFG_PUBLIC_KEY]);
        let required: Vec<&str> = spec
            .iter()
            .filter(|f| f.required)
            .map(|f| f.name())
            .collect();
        // Only the URL: a public repository on its default branch needs no key
        // and no branch, and demanding either would block the simplest case.
        assert_eq!(required, [CFG_URL]);
        assert!(spec.iter().all(|f| !f.base.label.is_empty()));

        // No `path` setting — where a clone goes is Saltcorn's decision, not a
        // question for the admin.
        assert!(!names.contains(&CFG_PATH));
    }

    #[test]
    fn a_git_store_without_a_url_is_rejected_on_save() {
        let def = FileStoreDef::new("app", GIT_BACKEND);
        let err = validate_file_store_config(&def).unwrap_err().to_string();
        assert!(err.contains("app"), "{err}");
        assert!(err.contains(CFG_URL), "{err}");
    }

    #[test]
    fn a_backends_operations_are_declared_like_its_settings() {
        // The `local` backend has none: there is nothing to do to a directory
        // that reading and writing files does not already cover. That is the
        // shape most backends have, and it is why operations are declared
        // rather than assumed.
        assert!(backend_operations(LOCAL_BACKEND).unwrap().is_empty());

        let ops = backend_operations(GIT_BACKEND).unwrap();
        let names: Vec<&str> = ops.iter().map(|op| op.name.as_str()).collect();
        assert_eq!(
            names,
            [
                crate::git::OP_GENERATE_KEY,
                crate::git::OP_STATUS,
                crate::git::OP_CLONE,
                crate::git::OP_PULL,
                crate::git::OP_PUSH,
                crate::git::OP_COMMIT,
            ]
        );
        // Every one carries a button label, because the admin UI renders this
        // and nothing else knows what these mean.
        assert!(ops.iter().all(|op| !op.label.is_empty()));

        // An unknown backend has no operations for the same reason it has no
        // settings spec: nothing implements it.
        assert!(backend_operations("s3").is_err());
    }

    #[tokio::test]
    async fn an_undeclared_operation_is_refused_by_name() {
        // The check is the registry's, not any backend's — which is what a
        // backend added later relies on.
        let mut def = FileStoreDef::local("docs", "/srv/docs");
        let err = run_backend_operation(&mut def, "pull", &Attrs::new())
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("pull"), "{err}");
        assert!(err.contains(LOCAL_BACKEND), "{err}");

        // A git store gets told what there *is*, since something is available.
        let mut def = FileStoreDef::git("app", "git@example.com:me/app.git");
        let err = run_backend_operation(&mut def, "rebase", &Attrs::new())
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("rebase"), "{err}");
        assert!(err.contains(crate::git::OP_PULL), "{err}");
    }

    #[tokio::test]
    async fn an_operations_arguments_are_validated_against_what_it_declared() {
        // A commit with no message is caught by the declared `required` field,
        // in the same place and with the same message shape a missing setting
        // is — not by anything that knows what a commit is.
        let mut def = FileStoreDef::git("app", "git@example.com:me/app.git");
        let err = run_backend_operation(&mut def, crate::git::OP_COMMIT, &Attrs::new())
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains(crate::git::ARG_MESSAGE), "{err}");
    }

    #[test]
    fn an_uncloned_git_store_does_not_connect_and_says_why() {
        let mut def = FileStoreDef::git("app", "git@example.com:me/app.git");
        // Structurally correct — the admin has configured it properly …
        assert!(validate_file_store_config(&def).is_ok());
        // … but there is no working tree, and connecting must not reach the
        // network to make one. The error names the store and says what is
        // missing, so the admin knows to clone rather than to re-check the URL.
        crate::git::record_clone_path(&mut def, std::path::Path::new("/definitely/not/here"));
        let err = connect_err(&def);
        assert!(err.contains("app"), "{err}");
        assert!(err.contains("clone"), "{err}");
    }

    #[test]
    fn an_unknown_backend_has_no_spec_and_says_what_is_available() {
        let err = backend_config_spec("s3").unwrap_err().to_string();
        assert!(err.contains("s3"), "{err}");
        assert!(err.contains(LOCAL_BACKEND), "should say what is available");
    }

    #[test]
    fn a_missing_required_setting_names_the_store_the_backend_and_the_setting() {
        let def = FileStoreDef::new("docs", LOCAL_BACKEND);
        let err = validate_file_store_config(&def).unwrap_err().to_string();
        assert!(err.contains("docs"), "{err}");
        assert!(err.contains(LOCAL_BACKEND), "{err}");
        assert!(err.contains(CFG_PATH), "{err}");
        // One "invalid:" prefix, not two — the message is rebuilt, not nested.
        assert_eq!(err.matches("invalid:").count(), 1, "{err}");
    }

    #[test]
    fn an_ill_typed_or_unknown_setting_is_rejected() {
        // `path` must be text, not a number.
        let def = FileStoreDef::new("docs", LOCAL_BACKEND).with(CFG_PATH, 42);
        let err = validate_file_store_config(&def).unwrap_err().to_string();
        assert!(err.contains(CFG_PATH), "{err}");

        // A setting the backend has never heard of is a typo, not something to
        // store and ignore.
        let def = FileStoreDef::local("docs", "/srv/docs").with("pth", "/srv/docs");
        let err = validate_file_store_config(&def).unwrap_err().to_string();
        assert!(err.contains("pth"), "{err}");
    }

    #[test]
    fn a_valid_definition_validates_without_touching_the_filesystem() {
        // The path does not exist and never will; structural validation must
        // still pass, because reachability is not a save-time question. This is
        // what keeps a store whose disk was unmounted editable.
        let def = FileStoreDef::local("gone", "/definitely/not/here");
        assert!(validate_file_store_config(&def).is_ok());
    }

    #[test]
    fn connecting_yields_a_working_store() {
        let dir = temp_path("connect");
        std::fs::create_dir_all(&dir).unwrap();

        let def = FileStoreDef::local("docs", dir.to_string_lossy());
        let store = connect_from_def(&def).unwrap();
        // The instance is named after the definition, which is the key the
        // catalog registry connects it under.
        assert_eq!(store.name(), "docs");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_unreachable_path_fails_to_connect_but_is_not_a_config_error() {
        let def = FileStoreDef::local("gone", "/definitely/not/here");
        // Structurally fine …
        assert!(validate_file_store_config(&def).is_ok());
        // … but not currently usable. The error names the store so a boot log
        // listing several failures is readable.
        let err = connect_err(&def);
        assert!(err.contains("gone"), "{err}");
    }

    #[test]
    fn create_makes_the_directory_only_when_asked() {
        let dir = temp_path("create");
        assert!(!dir.exists());

        // Without `create`, a missing directory is a failure to connect — a
        // typo'd path must not silently become a new empty store.
        let plain = FileStoreDef::local("new", dir.to_string_lossy());
        assert!(connect_from_def(&plain).is_err());
        assert!(!dir.exists(), "connecting must not have created it");

        // With it, the directory is created and the store connects.
        let creating = FileStoreDef::local("new", dir.to_string_lossy()).with(CFG_CREATE, true);
        let store = connect_from_def(&creating).unwrap();
        assert_eq!(store.name(), "new");
        assert!(dir.is_dir());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn connecting_validates_first() {
        // A structurally broken definition gets the clear configuration error,
        // not a confusing filesystem one.
        let def = FileStoreDef::new("docs", "s3").with("bucket", "things");
        let err = connect_err(&def);
        assert!(err.contains("s3"), "{err}");
    }
}
