//! The checked-in admin client (`ui/admin/src/client.ts`) is a **generated**
//! artifact and must not drift from the endpoint contract it is generated from.
//!
//! The SPA consumes `client.ts` directly, so if someone changes an admin
//! endpoint without regenerating the client, the SPA would be typed against a
//! stale contract. This test regenerates the client from
//! [`sc_api::admin_endpoints`] and asserts it byte-for-byte equals the committed
//! file, pointing at the one command that refreshes it.

use std::path::PathBuf;

#[test]
fn committed_admin_client_matches_generator() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../ui/admin/src/client.ts");
    let committed =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let generated = sc_api::generate_client(&sc_api::admin_endpoints());

    assert_eq!(
        committed, generated,
        "ui/admin/src/client.ts is stale. Regenerate it with:\n  \
         cargo run -p sc-api --example emit_admin_client -- ui/admin/src/client.ts"
    );
}
