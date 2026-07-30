//! The checked-in admin clients (`ui/admin/src/client.ts` and
//! `ui/ide/src/client.ts`) are **generated** artifacts and must not drift from the
//! endpoint contract they are generated from.
//!
//! Both bundles consume `client.ts` directly, so if someone changes an admin
//! endpoint without regenerating them, they would be typed against a stale
//! contract. This test regenerates the client from [`sc_api::admin_endpoints`] and
//! asserts each committed file byte-for-byte equals the output, pointing at the one
//! command that refreshes it.
//!
//! There are two copies rather than one shared file because the IDE is a separate
//! project with its own `tsconfig.json` and no import path into the SPA's sources
//! (design §12.1); this test is what keeps the copies honest.

use std::path::PathBuf;

#[test]
fn committed_admin_clients_match_generator() {
    let generated = sc_api::generate_client(&sc_api::admin_endpoints());
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");

    for ui in ["ui/admin", "ui/ide"] {
        let path = root.join(ui).join("src/client.ts");
        let committed = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        assert_eq!(
            committed, generated,
            "{ui}/src/client.ts is stale. Regenerate it with:\n  \
             cargo run -p sc-api --example emit_admin_client -- {ui}/src/client.ts"
        );
    }
}
