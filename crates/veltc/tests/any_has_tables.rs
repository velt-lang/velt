//! The prototype tables `key in x` through `any` answers from (velt_vir's any_has_builtin.rs)
//! agree with the built-ins of the Node on PATH (`scripts/any-has-tables.mjs`). Skipped without
//! `node`.

use std::path::Path;
use std::process::Command;

#[test]
fn any_has_tables_match_node() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let script = root.join("scripts/any-has-tables.mjs");
    let Ok(o) = Command::new("node").arg(&script).output() else {
        eprintln!("skipped: `node` is not on PATH");
        return;
    };
    let text = String::from_utf8_lossy(&o.stdout);
    eprintln!("{text}");
    assert!(
        o.status.success(),
        "the tables differ from Node's built-ins:\n{text}{}",
        String::from_utf8_lossy(&o.stderr)
    );
}
