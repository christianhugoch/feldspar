//! The generated TypeScript client must type-check against the declared
//! endpoints (design §13.1; Phase 6 "typed client" tasks).
//!
//! This exercises the full contract: it generates the client from
//! [`sc_api::admin_endpoints`], writes it next to a strict-mode usage file that
//! calls a representative set of endpoints with correctly-typed arguments, and
//! runs the TypeScript compiler in `--noEmit --strict` mode. A drift between a
//! declared endpoint and the generated call (wrong arg shape, wrong return type)
//! is a `tsc` error, so a green run proves the client is genuinely typed rather
//! than `any`.
//!
//! `tsc` is not always present, so the test resolves a compiler from (in order)
//! the `SC_TSC` env var or a `node_modules/.bin/tsc` under the crate, and
//! **skips** (rather than fails) when none is available — mirroring how the
//! DB-backed tests gate on a configured database. Run it locally with, e.g.:
//!
//! ```text
//! npm install typescript@5 --no-save --prefix crates/sc-api
//! cargo test -p sc-api --test typescript_typecheck
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;

/// A usage module that calls a cross-section of the admin endpoints with
/// correctly-typed arguments and consumes their typed results. It fails to
/// compile if the generated client drifts from the declared endpoints.
const USAGE_TS: &str = r#"
import { createClient, LoginResponse } from "./client";

export async function exercise(): Promise<void> {
  const api = createClient({ baseUrl: "http://localhost:3000" });

  // No-body GET returning a struct with a nested optional.
  const status = await api.authStatus();
  const exists: boolean = status.any_user_exists;

  // POST with a typed body and typed response.
  const user: LoginResponse = await api.login({ email: "a@b.c", password: "pw" });
  const role: number = user.role;

  // Path-parameter endpoints.
  const rows = await api.listRows("mytable");
  await api.updateRow("mytable", "some-id", { any: "json" });
  await api.deleteRow("mytable", "some-id");

  void exists; void role; void rows;
}
"#;

#[test]
fn generated_client_type_checks() {
    let Some(tsc) = resolve_tsc() else {
        eprintln!(
            "skipping: no TypeScript compiler found. Set SC_TSC=/path/to/tsc or run \
             `npm install typescript@5 --no-save --prefix crates/sc-api`."
        );
        return;
    };

    let client_ts = sc_api::generate_client(&sc_api::admin_endpoints());

    let dir = std::env::temp_dir().join(format!("sc-api-tsc-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    std::fs::write(dir.join("client.ts"), &client_ts).expect("write client.ts");
    std::fs::write(dir.join("usage.ts"), USAGE_TS).expect("write usage.ts");

    let output = Command::new(&tsc)
        .args([
            "--noEmit",
            "--strict",
            "--lib",
            "es2020,dom",
            "--moduleResolution",
            "node",
            "--module",
            "esnext",
        ])
        .arg(dir.join("client.ts"))
        .arg(dir.join("usage.ts"))
        .output()
        .unwrap_or_else(|e| panic!("failed to run tsc at {}: {e}", tsc.display()));

    let _ = std::fs::remove_dir_all(&dir);

    assert!(
        output.status.success(),
        "generated TypeScript failed to type-check:\n--- stdout ---\n{}\n--- stderr ---\n{}\n--- client.ts ---\n{client_ts}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

/// Resolve a TypeScript compiler: `SC_TSC`, else a `node_modules/.bin/tsc`
/// installed under the crate. Returns `None` when neither is present.
fn resolve_tsc() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("SC_TSC") {
        let p = PathBuf::from(p);
        if p.exists() {
            return Some(p);
        }
    }
    let local = Path::new(env!("CARGO_MANIFEST_DIR")).join("node_modules/.bin/tsc");
    local.exists().then_some(local)
}
