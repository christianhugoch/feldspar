//! What a module's worker may reach: the permission set stored beside it, and
//! the sentence a denial is reported with (TODO "Modules in-process", phase 3;
//! specification §2).
//!
//! §15.1 used to end "Installing a module runs arbitrary code as the server …
//! There is no sandbox", and that sentence was a consequence of `node`, which
//! has no permission model to ask for. Deno does, so a module's worker is
//! handed a [`deno_permissions::PermissionsContainer`] built from *this* — a net
//! allow-list, a readable and a writable path list, and the environment
//! variables it may see.
//!
//! **Closed by default.** A module installed with nothing declared reaches
//! nothing: no host, no path, no variable. Widening it is an admin action on the
//! Modules tab, one entry at a time, and never a flag that quietly defaults
//! open — a permission set is only as good as what defaults to it.
//!
//! ## The two honesty constraints, kept here because they are easy to lose
//!
//! - **`npm install` is still unsandboxed.** Install scripts run as the server,
//!   before any worker exists. Nothing in this file changes that half, and the
//!   Modules tab says so beside the permissions rather than letting the presence
//!   of a permission screen imply otherwise.
//! - **Three capabilities are never granted at all**: subprocesses (`run`),
//!   native FFI, and dynamic `import` of anything off the disk. They have no
//!   entry here because there is no allow-list to put them on — a module that
//!   needs to run a program needs a different Saltcorn.
//!
//! ## What a closed module can still do, and why it must
//!
//! **Read its own code.** `require` walks the modules root, and a module that
//! may not read the package it is made of is a module that cannot exist. That
//! read is allowed by the require loader itself
//! ([`crate::deno`]'s `ensure_read_permission`), *not* by the container — so
//! `node:fs` reading the same directory is still denied. The distinction is
//! deliberate: the code a module is made of is not a capability, and everything
//! else on the filesystem is.
//!
//! **Read the environment — as nothing.** A denied `process.env.NODE_ENV` is
//! `undefined` rather than a throw, because half of npm reads a variable
//! speculatively at load time and turning that into a crash would be a rule that
//! most modules may not be installed. Deno's own `ignore` state is what does it.
//! A granted variable reads normally. Net, read and write denials are **errors**
//! by contrast, because there is something for an admin to do about each of
//! them and a silent failure would hide it.

use sc_error::{Error, Result};
use sc_types::Attrs;
use serde_json::{Value as Json, json};

/// The keys the stored JSON object uses, which are also the field names the API
/// and the Modules tab speak.
pub const PERM_NET: &str = "net";
/// Readable paths.
pub const PERM_READ: &str = "read";
/// Writable paths.
pub const PERM_WRITE: &str = "write";
/// Environment variables.
pub const PERM_ENV: &str = "env";

/// What one module's worker may reach.
///
/// Every list is an allow-list and every empty list means *nothing*, never
/// *everything* — which is the one place this type could have gone wrong, and
/// the reason [`ModulePermissions::closed`] is what [`Default`] answers.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct ModulePermissions {
    /// Hosts the module may open a socket to: `broker.example` (any port) or
    /// `broker.example:1883` (that port only).
    pub net: Vec<String>,
    /// Absolute paths the module may read. A directory allows what is under it.
    pub read: Vec<String>,
    /// Absolute paths the module may write.
    pub write: Vec<String>,
    /// Environment variable names the module may see.
    pub env: Vec<String>,
}

impl ModulePermissions {
    /// The set a module gets when nobody has granted it anything.
    pub fn closed() -> ModulePermissions {
        ModulePermissions::default()
    }

    /// Whether nothing at all is granted.
    pub fn is_closed(&self) -> bool {
        self.net.is_empty() && self.read.is_empty() && self.write.is_empty() && self.env.is_empty()
    }

    /// How many entries there are across every list — what a badge counts.
    pub fn granted(&self) -> usize {
        self.net.len() + self.read.len() + self.write.len() + self.env.len()
    }

    /// Read a stored (or posted) permission set, checking every entry.
    ///
    /// Strict, in the way [`crate::store`] is strict about a row: an entry that
    /// is not a string, a path that is not absolute, a host with a scheme on it
    /// are each an [`Error::invalid`] naming the entry. A permission set nobody
    /// can read exactly is one that would be applied approximately.
    pub fn from_json(value: &Json) -> Result<ModulePermissions> {
        let object = match value {
            Json::Object(object) => object,
            Json::Null => return Ok(ModulePermissions::closed()),
            other => {
                return Err(Error::invalid(format!(
                    "a module's permissions should be an object, got {}",
                    kind_of(other)
                )));
            }
        };
        for key in object.keys() {
            if ![PERM_NET, PERM_READ, PERM_WRITE, PERM_ENV].contains(&key.as_str()) {
                return Err(Error::invalid(format!(
                    "`{key}` is not a module permission; the permissions are {PERM_NET}, \
                     {PERM_READ}, {PERM_WRITE}, {PERM_ENV}"
                )));
            }
        }
        let permissions = ModulePermissions {
            net: entries(object, PERM_NET, check_host)?,
            read: entries(object, PERM_READ, check_path)?,
            write: entries(object, PERM_WRITE, check_path)?,
            env: entries(object, PERM_ENV, check_env)?,
        };
        Ok(permissions)
    }

    /// The set as it is stored and as the API reports it.
    pub fn to_json(&self) -> Attrs {
        let mut object = Attrs::new();
        object.insert(PERM_NET.to_owned(), json!(self.net));
        object.insert(PERM_READ.to_owned(), json!(self.read));
        object.insert(PERM_WRITE.to_owned(), json!(self.write));
        object.insert(PERM_ENV.to_owned(), json!(self.env));
        object
    }

    /// One sentence per granted capability, for a log line and for the admin's
    /// own reading. Empty when nothing is granted, so the caller says "nothing"
    /// in its own words rather than rendering an empty list.
    pub fn sentences(&self) -> Vec<String> {
        let mut lines = Vec::new();
        if !self.net.is_empty() {
            lines.push(format!("may connect to {}", self.net.join(", ")));
        }
        if !self.read.is_empty() {
            lines.push(format!("may read {}", self.read.join(", ")));
        }
        if !self.write.is_empty() {
            lines.push(format!("may write {}", self.write.join(", ")));
        }
        if !self.env.is_empty() {
            lines.push(format!("may read the variables {}", self.env.join(", ")));
        }
        lines
    }
}

/// One list of the object, checked entry by entry and sorted so that two sets
/// granting the same things are the *same* set.
///
/// Sorting is not tidiness: the pool pins a module to a worker by its permission
/// set ([`crate::deno`]), so two modules that were granted the same host in a
/// different order must share a worker rather than each getting one.
fn entries(
    object: &Attrs,
    key: &str,
    check: impl Fn(&str) -> Result<String>,
) -> Result<Vec<String>> {
    let Some(value) = object.get(key) else {
        return Ok(Vec::new());
    };
    let list = match value {
        Json::Array(list) => list,
        Json::Null => return Ok(Vec::new()),
        other => {
            return Err(Error::invalid(format!(
                "a module's `{key}` permission should be a list, got {}",
                kind_of(other)
            )));
        }
    };
    let mut out = Vec::with_capacity(list.len());
    for entry in list {
        let Json::String(text) = entry else {
            return Err(Error::invalid(format!(
                "a module's `{key}` permission should be a list of strings, and one entry is {}",
                kind_of(entry)
            )));
        };
        out.push(check(text.trim())?);
    }
    out.sort();
    out.dedup();
    Ok(out)
}

/// A host, or a host and a port. Not a URL: what the permission checks match is
/// the socket's address, and `https://host/path` would silently match nothing.
fn check_host(entry: &str) -> Result<String> {
    if entry.is_empty() {
        return Err(Error::invalid("a network permission needs a host"));
    }
    if entry.contains("://") || entry.contains('/') {
        return Err(Error::invalid(format!(
            "`{entry}` is a URL; a network permission is a host or a host:port, like \
             `broker.example` or `broker.example:1883`"
        )));
    }
    // A trailing `:1883` is a port; a trailing `]` is an IPv6 address with no
    // port on it, which is a host and nothing more.
    if let Some((host, port)) = entry.rsplit_once(':')
        && !host.is_empty()
        && !host.ends_with(']')
    {
        match port.parse::<u16>() {
            Ok(0) | Err(_) => {
                return Err(Error::invalid(format!(
                    "`{port}` in `{entry}` is not a port number"
                )));
            }
            Ok(_) => {}
        }
    }
    Ok(entry.to_owned())
}

/// An absolute path, because a relative one means whatever directory the server
/// happened to start in — which is not a thing an admin can reason about.
fn check_path(entry: &str) -> Result<String> {
    if entry.is_empty() {
        return Err(Error::invalid("a filesystem permission needs a path"));
    }
    if !std::path::Path::new(entry).is_absolute() {
        return Err(Error::invalid(format!(
            "`{entry}` is not an absolute path, so it would mean a different directory \
             depending on where the server was started"
        )));
    }
    Ok(entry.to_owned())
}

/// A variable name.
fn check_env(entry: &str) -> Result<String> {
    if entry.is_empty() {
        return Err(Error::invalid("an environment permission needs a name"));
    }
    if entry.contains('=') || entry.contains('\0') {
        return Err(Error::invalid(format!(
            "`{entry}` is not an environment variable name"
        )));
    }
    Ok(entry.to_owned())
}

fn kind_of(value: &Json) -> &'static str {
    match value {
        Json::Null => "null",
        Json::Bool(_) => "a boolean",
        Json::Number(_) => "a number",
        Json::String(_) => "a string",
        Json::Array(_) => "a list",
        Json::Object(_) => "an object",
    }
}

/// Deno's own denial, rewritten as something an admin can act on.
///
/// A denial arrives as the module's own thrown error, from wherever inside its
/// dependency tree the socket or the file was reached for, and Deno words it for
/// somebody holding a command line: *Requires net access to "broker:1883", run
/// again with the --allow-net flag*. There is no command line here, so the
/// sentence names the screen instead — and says which list the entry goes on,
/// and what to type in it.
///
/// Returns `None` for anything that is not a permission denial, so the caller
/// leaves every other failure exactly as the module worded it.
pub fn explain_denial(module: &str, message: &str) -> Option<String> {
    // `Requires <access>, run again with the --allow-<name> flag`, or the
    // `deno compile` variant of the same sentence.
    let (before, after) = message.split_once(", run again with the --allow-")?;
    // From "Requires " rather than from the start of the string: what arrives
    // here is an `Error`'s own `Display`, which has already put its kind in
    // front of the module's words.
    let at = before.rfind("Requires ")?;
    let access = before[at + "Requires ".len()..].trim();
    let name = after.split_whitespace().next()?.trim_end_matches("flag");
    let name = name.trim_end_matches(|c: char| !c.is_ascii_alphabetic());
    let what = access.rsplit_once(" to ").map_or(access, |(_, what)| what);
    let remedy = match name {
        PERM_NET => format!(
            "add {what} to its network allow-list in Settings → Modules → {module} → Permissions"
        ),
        PERM_READ => format!(
            "add {what} to its readable paths in Settings → Modules → {module} → Permissions"
        ),
        PERM_WRITE => format!(
            "add {what} to its writable paths in Settings → Modules → {module} → Permissions"
        ),
        PERM_ENV => format!(
            "add {what} to its environment variables in Settings → Modules → {module} → \
             Permissions"
        ),
        other => format!(
            "Saltcorn never grants a module {other} access, so this module cannot do that on \
             this server"
        ),
    };
    Some(format!(
        "the module `{module}` was denied {access}: {remedy}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_declared_is_nothing_granted() {
        let permissions = ModulePermissions::from_json(&json!({})).unwrap();
        assert!(permissions.is_closed());
        assert_eq!(permissions, ModulePermissions::closed());
        assert_eq!(ModulePermissions::default(), ModulePermissions::closed());
        // And a row that has never been written reads as closed rather than as
        // an error: a module installed before this column existed is denied,
        // not broken.
        assert!(
            ModulePermissions::from_json(&Json::Null)
                .unwrap()
                .is_closed()
        );
    }

    #[test]
    fn a_set_round_trips_through_its_stored_json() {
        let permissions = ModulePermissions {
            net: vec!["broker.example:1883".into()],
            read: vec!["/srv/data".into()],
            write: Vec::new(),
            env: vec!["MQTT_PASSWORD".into()],
        };
        let stored = Json::Object(permissions.to_json());
        assert_eq!(ModulePermissions::from_json(&stored).unwrap(), permissions);
    }

    #[test]
    fn two_sets_granting_the_same_things_are_one_set() {
        // The pool pins by permission set, so this is the difference between two
        // modules sharing a worker and each getting one.
        let one = ModulePermissions::from_json(&json!({ "net": ["a:1", "b:2"] })).unwrap();
        let other = ModulePermissions::from_json(&json!({ "net": ["b:2", "a:1", "a:1"] })).unwrap();
        assert_eq!(one, other);
    }

    #[test]
    fn a_url_is_not_a_network_permission() {
        let err = ModulePermissions::from_json(&json!({ "net": ["https://broker.example"] }))
            .unwrap_err()
            .to_string();
        assert!(err.contains("host:port"), "{err}");
    }

    #[test]
    fn a_relative_path_is_refused_because_it_names_nowhere_in_particular() {
        let err = ModulePermissions::from_json(&json!({ "read": ["data"] }))
            .unwrap_err()
            .to_string();
        assert!(err.contains("absolute"), "{err}");
    }

    #[test]
    fn an_unknown_permission_names_the_ones_there_are() {
        let err = ModulePermissions::from_json(&json!({ "run": ["ls"] }))
            .unwrap_err()
            .to_string();
        assert!(err.contains("run"), "{err}");
        assert!(err.contains("net") && err.contains("env"), "{err}");
    }

    #[test]
    fn a_bad_port_says_so() {
        let err = ModulePermissions::from_json(&json!({ "net": ["broker:eighteen"] }))
            .unwrap_err()
            .to_string();
        assert!(err.contains("port"), "{err}");
    }

    #[test]
    fn a_denial_names_the_module_the_capability_and_the_screen() {
        let sentence = explain_denial(
            "@saltcorn/mqtt",
            "Requires net access to \"broker.example:1883\", run again with the --allow-net flag",
        )
        .expect("a net denial should be recognised");
        assert!(sentence.contains("@saltcorn/mqtt"), "{sentence}");
        assert!(sentence.contains("broker.example:1883"), "{sentence}");
        assert!(sentence.contains("network allow-list"), "{sentence}");
        assert!(sentence.contains("Settings → Modules"), "{sentence}");
        // And nothing that reads as a command line an admin does not have.
        assert!(!sentence.contains("--allow-net"), "{sentence}");
    }

    #[test]
    fn a_denial_is_recognised_after_an_error_has_put_its_kind_in_front() {
        // What reaches this function is an `Error`'s `Display`, not the raw
        // throw: "configuration error: Requires …". Recognising only the raw
        // form would mean every real denial reached the admin as Deno's
        // command-line sentence.
        let sentence = explain_denial(
            "@saltcorn/mqtt",
            "configuration error: Requires net access to \"127.0.0.1:1883\", run again with the \
             --allow-net flag",
        )
        .expect("a wrapped net denial should be recognised");
        assert!(sentence.contains("127.0.0.1:1883"), "{sentence}");
    }

    #[test]
    fn a_capability_with_no_allow_list_says_it_is_never_granted() {
        let sentence = explain_denial(
            "@saltcorn/mqtt",
            "Requires run access to \"ls\", run again with the --allow-run flag",
        )
        .expect("a run denial should be recognised");
        assert!(sentence.contains("never grants"), "{sentence}");
    }

    #[test]
    fn anything_that_is_not_a_denial_is_left_alone() {
        assert!(explain_denial("@saltcorn/mqtt", "connect ECONNREFUSED").is_none());
    }

    #[test]
    fn the_granted_capabilities_read_as_sentences() {
        let permissions = ModulePermissions {
            net: vec!["broker.example:1883".into()],
            read: Vec::new(),
            write: Vec::new(),
            env: vec!["HOME".into()],
        };
        let lines = permissions.sentences();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("broker.example:1883"));
        assert_eq!(permissions.granted(), 2);
        assert!(ModulePermissions::closed().sentences().is_empty());
    }
}
