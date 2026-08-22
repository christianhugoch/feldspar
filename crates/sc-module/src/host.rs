//! The Node child process modules run in, and the line protocol it speaks.
//!
//! One child for every module, not one each: a module is a few hundred kilobytes
//! of JavaScript and a socket, and a process per module would buy isolation
//! nobody asked for at the price of a process table full of them.
//!
//! **Lazily started**: a deployment with no modules never spawns it, and never
//! needs `node` installed. **Restarted on death**: a module that calls
//! `process.exit()` takes its co-residents' in-flight calls with it, so each of
//! those is failed by name and the next call starts a fresh process — which
//! replays every load this host has performed, so the module set survives a
//! crash without the caller knowing there was one.
//!
//! The protocol is newline-delimited JSON ([`js/module-host.mjs`]): a request
//! carries an `id` and the reply carries it back, so many calls are in flight at
//! once and a slow module's action does not hold anybody else's. `stderr` is the
//! modules' own logging and is forwarded to the server's.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use sc_error::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{Mutex, oneshot};

/// The host script, written into the modules root at every start.
///
/// `pub(crate)` because the in-process runtime ([`crate::deno`]) writes and runs
/// the very same script, unedited: what phase 0 proved portable was this text,
/// and a runtime change that also rewrote it could not say which half broke.
pub(crate) const HOST_SCRIPT: &str = include_str!("js/module-host.mjs");

/// What the host script is called on disk.
pub const HOST_SCRIPT_NAME: &str = "module-host.mjs";

pub use crate::bounds::DEFAULT_CALL_TIMEOUT;

/// One action, as the module declared it.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ActionManifest {
    /// The name the action is registered under — v1's own, unqualified.
    pub name: String,
    /// The module's one-line description, if it gave one.
    #[serde(default)]
    pub description: String,
    /// Whether the action needs a row to act on.
    #[serde(default, rename = "requireRow")]
    pub require_row: bool,
    /// v1 `configFields`, as the module declared them — translated by
    /// [`crate::spec`], never interpreted here.
    #[serde(default, rename = "configFields")]
    pub config_fields: Vec<Json>,
}

/// An entity type the module exports and this version does not load.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct UnsupportedEntity {
    /// The plugin key — `viewtemplates`, `table_providers`, `eventTypes`.
    pub key: String,
    /// How many of them, when that can be told without running the module's
    /// code.
    #[serde(default)]
    pub count: Option<u64>,
}

/// What a module turned out to supply.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ModuleManifest {
    /// The package name it was loaded under.
    pub name: String,
    /// v1's `sc_plugin_api_version`, when it declares one.
    #[serde(default)]
    pub api_version: Option<u64>,
    /// v1's `plugin_name`, when it declares one.
    #[serde(default)]
    pub plugin_name: Option<String>,
    /// The actions it supplies.
    #[serde(default)]
    pub actions: Vec<ActionManifest>,
    /// The fields of its `configuration_workflow`'s forms, flattened (§5).
    #[serde(default)]
    pub config_fields: Vec<Json>,
    /// What it also supplies and this version does not load (§6).
    #[serde(default)]
    pub unsupported: Vec<UnsupportedEntity>,
    /// What went wrong that was not fatal — a step's form that would not build,
    /// an action whose `configFields` threw.
    #[serde(default)]
    pub issues: Vec<String>,
}

/// A load, remembered so it can be replayed into a restarted process.
#[derive(Debug, Clone)]
struct LoadRequest {
    dir: PathBuf,
    configuration: Json,
}

/// The Node sidecar: spawn, protocol, and restart.
pub struct ModuleHost {
    /// The modules root — the host's working directory, and where the script is
    /// written.
    root: PathBuf,
    /// How long a call may take.
    timeout: std::time::Duration,
    /// The running process, or `None` before the first call and after a death.
    process: Mutex<Option<HostProcess>>,
    /// The loads to replay into a restarted process, by module name.
    loads: Mutex<BTreeMap<String, LoadRequest>>,
    /// The next request id.
    next_id: AtomicU64,
}

/// One running child and the calls waiting on it.
struct HostProcess {
    child: Child,
    stdin: ChildStdin,
    pending: Arc<std::sync::Mutex<HashMap<u64, oneshot::Sender<Result<Json>>>>>,
    alive: Arc<AtomicBool>,
}

impl ModuleHost {
    /// A host over the modules root. Nothing is spawned until the first call.
    pub fn new(root: impl Into<PathBuf>) -> ModuleHost {
        ModuleHost {
            root: root.into(),
            timeout: DEFAULT_CALL_TIMEOUT,
            process: Mutex::new(None),
            loads: Mutex::new(BTreeMap::new()),
            next_id: AtomicU64::new(1),
        }
    }

    /// A host whose calls are bounded by `timeout` rather than
    /// [`DEFAULT_CALL_TIMEOUT`].
    pub fn with_timeout(mut self, timeout: std::time::Duration) -> ModuleHost {
        self.timeout = timeout;
        self
    }

    /// The modules root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Load (or reload) a module from `dir`, with `configuration` as the object
    /// handed to v1's `actions(cfg)`.
    ///
    /// Idempotent: loading twice replaces, which is what a configuration change
    /// and a reinstall both do.
    pub async fn load(
        &self,
        name: &str,
        dir: &Path,
        configuration: &Json,
    ) -> Result<ModuleManifest> {
        let request = LoadRequest {
            dir: dir.to_path_buf(),
            configuration: configuration.clone(),
        };
        let value = self
            .call(json!({
                "op": "load",
                "module": name,
                "dir": request.dir.display().to_string(),
                "configuration": request.configuration,
            }))
            .await?;
        // Remembered only once it has worked: replaying a load that fails would
        // fail the same way at every restart, and the module is reported as
        // broken rather than pending.
        self.loads.lock().await.insert(name.to_owned(), request);
        serde_json::from_value(value).map_err(|e| {
            Error::msg(format!(
                "the module host answered a load of `{name}` with something unreadable: {e}"
            ))
        })
    }

    /// Forget a module — after an uninstall, so a restarted host does not
    /// reload a package that is no longer there.
    pub async fn unload(&self, name: &str) {
        self.loads.lock().await.remove(name);
        // Best effort: a host that is not running has already forgotten it.
        let _ = self.call(json!({ "op": "unload", "module": name })).await;
    }

    /// Run one action of one module with v1's argument object.
    pub async fn run(&self, module: &str, action: &str, args: Json) -> Result<Json> {
        self.call(json!({
            "op": "run",
            "module": module,
            "action": action,
            "args": args,
        }))
        .await
    }

    /// Ask the host to say hello — what a test and a diagnostics screen use to
    /// find out whether `node` is there at all.
    pub async fn ping(&self) -> Result<Json> {
        self.call(json!({ "op": "ping" })).await
    }

    /// Stop the child, if one is running.
    pub async fn shutdown(&self) {
        if let Some(mut process) = self.process.lock().await.take() {
            let _ = process.child.kill().await;
        }
    }

    /// Send one request and await its reply.
    async fn call(&self, mut request: Json) -> Result<Json> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let receiver = {
            // The lock spans the spawn and the write, so two concurrent callers
            // cannot both spawn a process and cannot interleave half-lines. It
            // does *not* span the await below, which is what keeps calls
            // concurrent.
            let mut guard = self.process.lock().await;
            self.ensure_started(&mut guard).await?;
            let Some(process) = guard.as_mut() else {
                return Err(Error::msg("the module host did not start"));
            };

            if let Some(object) = request.as_object_mut() {
                object.insert("id".into(), json!(id));
            }
            let (sender, receiver) = oneshot::channel();
            match process.pending.lock() {
                Ok(mut pending) => {
                    pending.insert(id, sender);
                }
                Err(_) => return Err(Error::msg("the module host's call table is poisoned")),
            }
            let mut line = serde_json::to_string(&request)
                .map_err(|e| Error::msg(format!("serialising a module-host request: {e}")))?;
            line.push('\n');
            if let Err(e) = process.stdin.write_all(line.as_bytes()).await {
                process.pending.lock().ok().and_then(|mut p| p.remove(&id));
                // The child is gone or its pipe is: drop it so the next call
                // starts a new one rather than writing into a closed pipe
                // forever.
                *guard = None;
                return Err(Error::config(format!(
                    "the module host could not be written to: {e}"
                )));
            }
            let _ = process.stdin.flush().await;
            receiver
        };

        match tokio::time::timeout(self.timeout, receiver).await {
            Ok(Ok(result)) => result,
            // The sender was dropped without a reply: the reader task noticed
            // the process die and cleared the table.
            Ok(Err(_)) => Err(Error::config(
                "the module host stopped before answering this call",
            )),
            Err(_) => Err(Error::config(format!(
                "a module call took longer than {:?} and was given up on",
                self.timeout
            ))),
        }
    }

    /// Spawn the child if there is not a live one, replaying every load into it.
    async fn ensure_started(&self, guard: &mut Option<HostProcess>) -> Result<()> {
        if let Some(process) = guard.as_ref()
            && process.alive.load(Ordering::SeqCst)
        {
            return Ok(());
        }
        if let Some(mut dead) = guard.take() {
            let _ = dead.child.kill().await;
        }

        *guard = Some(self.spawn().await?);
        // Replay the loads *while still holding the lock*, so no caller's `run`
        // can reach the fresh process before its modules are back. Failures are
        // reported and skipped: a module whose package went missing must not
        // stop the rest of the host from serving.
        let loads = self.loads.lock().await.clone();
        for (name, request) in loads {
            let id = self.next_id.fetch_add(1, Ordering::Relaxed);
            let line = json!({
                "id": id,
                "op": "load",
                "module": name,
                "dir": request.dir.display().to_string(),
                "configuration": request.configuration,
            });
            if let Some(process) = guard.as_mut() {
                let mut text = serde_json::to_string(&line)
                    .map_err(|e| Error::msg(format!("serialising a module-host request: {e}")))?;
                text.push('\n');
                if process.stdin.write_all(text.as_bytes()).await.is_err() {
                    sc_log::log_error!(
                        "saltcorn: the restarted module host would not accept a reload of `{name}`"
                    );
                }
                let _ = process.stdin.flush().await;
            }
        }
        Ok(())
    }

    /// Write the host script and start `node` on it.
    async fn spawn(&self) -> Result<HostProcess> {
        tokio::fs::create_dir_all(&self.root).await.map_err(|e| {
            Error::config(format!(
                "creating the modules directory {}: {e}",
                self.root.display()
            ))
        })?;
        let script = self.root.join(HOST_SCRIPT_NAME);
        tokio::fs::write(&script, HOST_SCRIPT)
            .await
            .map_err(|e| Error::config(format!("writing {}: {e}", script.display())))?;

        let mut child = Command::new("node")
            .arg(&script)
            .current_dir(&self.root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                Error::config(format!(
                    "could not start the module host (`node {}`): {e}. Running a module needs \
                     Node.js on this server's PATH",
                    script.display()
                ))
            })?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| Error::msg("the module host has no stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::msg("the module host has no stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| Error::msg("the module host has no stderr"))?;

        let pending: Arc<std::sync::Mutex<HashMap<u64, oneshot::Sender<Result<Json>>>>> =
            Arc::new(std::sync::Mutex::new(HashMap::new()));
        let alive = Arc::new(AtomicBool::new(true));

        // Replies in.
        {
            let pending = Arc::clone(&pending);
            let alive = Arc::clone(&alive);
            tokio::spawn(async move {
                let mut lines = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let line: String = line;
                    let Ok(reply) = serde_json::from_str::<Json>(&line) else {
                        sc_log::log_error!("saltcorn: unreadable module-host reply: {line}");
                        continue;
                    };
                    let Some(id) = reply.get("id").and_then(Json::as_u64) else {
                        continue;
                    };
                    let sender = pending.lock().ok().and_then(|mut p| p.remove(&id));
                    let Some(sender) = sender else { continue };
                    let _ = sender.send(reply_result(&reply));
                }
                // End of stream: the child is gone. Everything waiting on it
                // gets a named failure rather than the call timeout, and the
                // next call spawns a new process.
                alive.store(false, Ordering::SeqCst);
                if let Ok(mut waiting) = pending.lock() {
                    for (_, sender) in waiting.drain() {
                        let _ = sender.send(Err(Error::config(
                            "the module host exited while this call was in flight; \
                             it will be restarted for the next one",
                        )));
                    }
                }
            });
        }

        // The modules' own logging out.
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let line: String = line;
                sc_log::log_info!("saltcorn: module host: {line}");
            }
        });

        Ok(HostProcess {
            child,
            stdin,
            pending,
            alive,
        })
    }
}

/// One reply line, as a result.
///
/// A module's throw is an **Application** error (§16): the fault is in the
/// module or in how it was configured, not in Saltcorn, and the admin who
/// installed it is the one who can act.
///
/// `pub(crate)`: the in-process runtime speaks the same protocol and reads its
/// replies the same way.
pub(crate) fn reply_result(reply: &Json) -> Result<Json> {
    if reply.get("ok").and_then(Json::as_bool) == Some(true) {
        return Ok(reply.get("value").cloned().unwrap_or(Json::Null));
    }
    let message = reply
        .get("error")
        .and_then(Json::as_str)
        .unwrap_or("the module failed without saying why");
    Err(Error::config(message.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_successful_reply_is_its_value() {
        let reply = json!({"id": 1, "ok": true, "value": {"pong": true}});
        assert_eq!(reply_result(&reply).unwrap(), json!({"pong": true}));
    }

    #[test]
    fn a_failed_reply_carries_the_modules_own_message() {
        let reply = json!({"id": 1, "ok": false, "error": "connect ECONNREFUSED"});
        let err = reply_result(&reply).unwrap_err();
        assert!(err.to_string().contains("ECONNREFUSED"), "{err}");
        // The module's fault, not the server's: it is the admin who installed it
        // who can fix it (§16's split).
        assert_eq!(err.kind(), sc_error::ErrorKind::Application);
    }

    #[test]
    fn a_reply_with_no_value_is_null_rather_than_an_error() {
        let reply = json!({"id": 1, "ok": true});
        assert_eq!(reply_result(&reply).unwrap(), Json::Null);
    }

    #[test]
    fn the_host_script_is_carried_in_the_binary() {
        // The script is written from here at every start, so a checkout whose
        // `js/` directory moved would be a host that cannot start — worth one
        // assertion that the file is really compiled in.
        assert!(
            HOST_SCRIPT.contains("module-host"),
            "the script looks wrong"
        );
        assert!(HOST_SCRIPT.contains("@saltcorn/"), "the stubs are missing");
    }
}
