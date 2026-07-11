//! Emit the admin UI's generated TypeScript client.
//!
//! The `ui/admin` SPA consumes a typed client generated from the admin endpoint
//! set (design §13.1). That client is a **generated** artifact: this example is
//! the single command that produces it, so the checked-in `ui/admin/src/client.ts`
//! never drifts from the Rust endpoint contract. A test
//! (`tests/admin_client_sync.rs`) asserts the committed file matches this output.
//!
//! Usage:
//!
//! ```text
//! # print to stdout
//! cargo run -p sc-api --example emit_admin_client
//! # or write to the SPA source tree
//! cargo run -p sc-api --example emit_admin_client -- ui/admin/src/client.ts
//! ```

use std::io::Write;

fn main() -> std::io::Result<()> {
    let client = sc_api::generate_client(&sc_api::admin_endpoints());
    match std::env::args().nth(1) {
        Some(path) => std::fs::write(path, client),
        None => std::io::stdout().write_all(client.as_bytes()),
    }
}
