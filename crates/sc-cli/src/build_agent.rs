//! `feldspar build-agent`: run this machine as a build host
//! (`TODO-build-hosts.md`, design §13.3 "Build hosts").
//!
//! The agent asks the server for work and the server never calls it, so the
//! machine (a Mac behind NAT, say) needs only outbound HTTPS. It needs no
//! database either: **a build host never generates code, it only builds.**
//!
//! B1 is the heartbeat. Every interval the agent tells the server who it is
//! and whether this machine has what each of its targets requires. The server
//! answers with the targets and their requirements, which the agent checks
//! here and reports at the next heartbeat. The first two go one after the
//! other, so the admin sees the readiness at once.
//!
//! **The token never goes on the command line**, where `ps` and the shell's
//! history would show it: it is read from `--token-file` or from
//! [`TOKEN_ENV`].

use std::path::PathBuf;
use std::time::Duration;

use reqwest::StatusCode;
use sc_app::{AgentReport, AnsweredTarget, HEARTBEAT_SECONDS, answered_targets};
use sc_error::{Error, Result};
use serde_json::Value as Json;

/// Where the token is read from when there is no `--token-file`.
pub const TOKEN_ENV: &str = "FELDSPAR_BUILD_TOKEN";

/// What `build-agent` was asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentArgs {
    /// `--server`: the server's address, `https://apps.example.com`.
    pub server: String,
    /// `--token-file`: a file holding the build host's token.
    pub token_file: Option<PathBuf>,
    /// `--once`: one heartbeat and its report, then exit — to check a setup.
    pub once: bool,
}

/// Read `build-agent`'s arguments.
pub fn parse_args(args: &[String]) -> Result<AgentArgs> {
    let mut server = None;
    let mut token_file = None;
    let mut once = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = |name: &str| {
            it.next()
                .cloned()
                .ok_or_else(|| Error::config(format!("{name} needs a value")))
        };
        match arg.as_str() {
            "--server" => server = Some(value("--server")?),
            "--token-file" => token_file = Some(PathBuf::from(value("--token-file")?)),
            "--once" => once = true,
            "--token" => {
                return Err(Error::config(format!(
                    "the token is not taken on the command line, where `ps` and the shell's \
                     history would show it: put it in a file and pass --token-file, or set \
                     {TOKEN_ENV}"
                )));
            }
            other => {
                return Err(Error::config(format!(
                    "unknown build-agent argument `{other}`; there are --server, \
                     --token-file and --once"
                )));
            }
        }
    }
    let server = server
        .ok_or_else(|| Error::config("build-agent needs --server, the server's address"))?
        .trim()
        .trim_end_matches('/')
        .to_owned();
    if !(server.starts_with("https://") || server.starts_with("http://")) {
        return Err(Error::config(format!(
            "--server `{server}` is not an address: it starts with https://"
        )));
    }
    Ok(AgentArgs {
        server,
        token_file,
        once,
    })
}

/// The build host's token: from `--token-file`, else from [`TOKEN_ENV`]
/// (`env` is its value, passed in so this is testable).
pub fn read_token(args: &AgentArgs, env: Option<String>) -> Result<String> {
    let token = match &args.token_file {
        Some(path) => std::fs::read_to_string(path).map_err(|e| {
            Error::config(format!("reading the token from {}: {e}", path.display()))
        })?,
        None => env.unwrap_or_default(),
    };
    let token = token.trim().to_owned();
    if token.is_empty() {
        return Err(Error::config(format!(
            "build-agent needs the build host's token, shown when the host was added under \
             Settings → Build hosts: in a file named by --token-file, or in {TOKEN_ENV}"
        )));
    }
    Ok(token)
}

/// How a heartbeat went wrong: worth retrying (the server is unreachable or
/// failed), or not (the token or the agent's version was refused).
#[derive(Debug)]
pub enum HeartbeatError {
    /// Try again at the next interval.
    Retry(Error),
    /// Stop: retrying cannot help until someone changes something.
    Fatal(Error),
}

/// What the server answered a heartbeat with.
#[derive(Debug, Clone, PartialEq)]
pub struct Answer {
    /// The host's name on the server.
    pub name: String,
    /// When to send the next heartbeat.
    pub interval: Duration,
    /// The targets the host builds, with what each requires.
    pub targets: Vec<AnsweredTarget>,
}

/// One build agent's conversation with its server.
pub struct Agent {
    client: reqwest::Client,
    url: String,
    token: String,
    targets: Vec<AnsweredTarget>,
}

impl Agent {
    /// An agent for `server` with `token`, which has not heard its targets yet.
    pub fn new(server: &str, token: String) -> Result<Agent> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| Error::msg(format!("making the HTTP client: {e}")))?;
        Ok(Agent {
            client,
            url: format!("{server}{}", sc_server::BUILD_AGENT_HEARTBEAT_ROUTE),
            token,
            targets: Vec::new(),
        })
    }

    /// Send one heartbeat: this machine's report against the targets heard so
    /// far. The answer's targets are kept for the next one.
    pub async fn heartbeat(&mut self) -> std::result::Result<Answer, HeartbeatError> {
        let report = AgentReport::of_this_machine(&self.targets);
        let response = self
            .client
            .post(&self.url)
            .bearer_auth(&self.token)
            .json(&report.to_json())
            .send()
            .await
            .map_err(|e| HeartbeatError::Retry(Error::msg(format!("{}: {e}", self.url))))?;
        let status = response.status();
        let body: Json = response.json().await.unwrap_or(Json::Null);
        let said = || {
            body.get("error")
                .and_then(Json::as_str)
                .map_or_else(|| format!("the server answered {status}"), str::to_owned)
        };
        match status {
            StatusCode::OK => {}
            StatusCode::UNAUTHORIZED | StatusCode::CONFLICT | StatusCode::NOT_FOUND => {
                return Err(HeartbeatError::Fatal(Error::config(said())));
            }
            _ => return Err(HeartbeatError::Retry(Error::msg(said()))),
        }
        let targets = answered_targets(&body).map_err(HeartbeatError::Fatal)?;
        self.targets = targets.clone();
        let seconds = body
            .get("interval_seconds")
            .and_then(Json::as_i64)
            .unwrap_or(HEARTBEAT_SECONDS)
            .max(1);
        Ok(Answer {
            name: body
                .get("name")
                .and_then(Json::as_str)
                .unwrap_or_default()
                .to_owned(),
            interval: Duration::from_secs(seconds.unsigned_abs()),
            targets,
        })
    }

    /// What this machine reports for its targets now, one line per target and
    /// one per thing missing: what the operator reads in the terminal.
    pub fn readiness_lines(&self) -> Vec<String> {
        let report = AgentReport::of_this_machine(&self.targets);
        let mut lines = Vec::new();
        for (target, (_, missing)) in self.targets.iter().zip(&report.readiness) {
            if missing.is_empty() {
                lines.push(format!("  {} ({}): ready", target.name, target.label));
            } else {
                lines.push(format!("  {} ({}): not ready", target.name, target.label));
                lines.extend(missing.iter().map(|m| format!("    - {m}")));
            }
        }
        lines
    }
}

/// `feldspar build-agent`: heartbeats until stopped, or one with `--once`.
///
/// A refused token or version stops the agent with the server's reason. An
/// unreachable server is retried at every interval, and said once, not at
/// every attempt.
pub async fn run(args: &AgentArgs) -> Result<()> {
    let token = read_token(args, std::env::var(TOKEN_ENV).ok())?;
    let mut agent = Agent::new(&args.server, token)?;
    // The first heartbeat hears the targets; the second reports on them.
    let fatal = |e: HeartbeatError| match e {
        HeartbeatError::Retry(e) | HeartbeatError::Fatal(e) => e,
    };
    agent.heartbeat().await.map_err(fatal)?;
    let answer = agent.heartbeat().await.map_err(fatal)?;
    eprintln!(
        "feldspar build-agent: connected to {} as `{}`",
        args.server, answer.name
    );
    let mut shown = agent.readiness_lines();
    for line in &shown {
        eprintln!("{line}");
    }
    if args.once {
        return Ok(());
    }
    let mut interval = answer.interval;
    let mut unreachable = false;
    loop {
        tokio::time::sleep(interval).await;
        match agent.heartbeat().await {
            Ok(answer) => {
                if unreachable {
                    eprintln!("feldspar build-agent: the server answers again");
                    unreachable = false;
                }
                interval = answer.interval;
                let lines = agent.readiness_lines();
                if lines != shown {
                    for line in &lines {
                        eprintln!("{line}");
                    }
                    shown = lines;
                }
            }
            Err(HeartbeatError::Retry(e)) => {
                if !unreachable {
                    eprintln!(
                        "feldspar build-agent: {e}; retrying every {}s",
                        interval.as_secs()
                    );
                    unreachable = true;
                }
            }
            Err(HeartbeatError::Fatal(e)) => return Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn the_server_is_required_and_the_token_is_never_an_argument() {
        let parsed =
            parse_args(&args(&["--server", "https://apps.example.com/", "--once"])).unwrap();
        assert_eq!(parsed.server, "https://apps.example.com");
        assert!(parsed.once);
        assert!(parse_args(&args(&["--once"])).is_err());
        assert!(parse_args(&args(&["--server", "apps.example.com"])).is_err());
        let why = parse_args(&args(&["--server", "https://a", "--token", "fsbh_x"]))
            .unwrap_err()
            .to_string();
        assert!(
            why.contains("--token-file") && why.contains(TOKEN_ENV),
            "{why}"
        );
    }

    #[test]
    fn the_token_comes_from_its_file_or_else_the_environment() {
        let dir = std::env::temp_dir().join(format!("build-agent-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("token");
        std::fs::write(&file, "fsbh_from_file\n").unwrap();
        let mut parsed = parse_args(&args(&["--server", "https://a"])).unwrap();
        assert_eq!(
            read_token(&parsed, Some("fsbh_env".into())).unwrap(),
            "fsbh_env"
        );
        assert!(read_token(&parsed, None).is_err());
        parsed.token_file = Some(file);
        assert_eq!(
            read_token(&parsed, Some("fsbh_env".into())).unwrap(),
            "fsbh_from_file"
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
