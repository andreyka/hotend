//! The committed TypeScript binding is what the command table generates.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

/// Fails when `src/lib/core/bindings.ts` is stale. `HOTEND_REGENERATE_BINDINGS=1` (what
/// `pnpm bindings` sets) writes the file instead.
#[test]
fn the_committed_typescript_binding_is_current() {
    let committed = Path::new(env!("CARGO_MANIFEST_DIR")).join("../src/lib/core/bindings.ts");
    let scratch = tempfile::tempdir().unwrap();
    let fresh = scratch.path().join("bindings.ts");
    hotend_desktop::ipc::export_bindings(&fresh).expect("the binding can be generated");
    let generated = std::fs::read_to_string(&fresh).unwrap();
    if std::env::var_os("HOTEND_REGENERATE_BINDINGS").is_some() {
        std::fs::write(&committed, &generated).unwrap();
        return;
    }
    let current = std::fs::read_to_string(&committed).unwrap_or_default();
    assert!(
        current == generated,
        "apps/desktop/src/lib/core/bindings.ts is stale: run `pnpm bindings` in apps/desktop \
         and commit the result"
    );
}
