//! `VELT_SEMA_CLONE_CHECK` builds every program also from a clone of sema's checked context and
//! compares the two (velt_sema `program.rs`): the dev host's kept checking context
//! (docs/internals/design/dev-warm-front-end.md step 3, "C") is a clone, sound only if a clone
//! is a complete, independent copy. Programs with classes, closures, generics, async code,
//! decorators, generated functions and JSON build with the check on, to the same VIR as without.
//! Every golden passes with it on too, which the merge queue doesn't run on each pull request.

mod no_window;
mod test_dir;

use std::path::Path;

/// `velt build <golden> --emit vir` with the clone check on or off: the VIR.
fn vir(golden: &str, check: bool) -> Vec<u8> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let home = test_dir::TestDir::new();
    let mut cmd = no_window::command(env!("CARGO_BIN_EXE_velt"));
    cmd.current_dir(&root)
        .args(["build", golden, "--emit", "vir"])
        .env("VELT_HOME", home.path())
        .env("VELT_BUILD_CACHE", "off")
        .env("VELT_BUILD_STAMP", "off")
        .env_remove("VELT_SEMA_CLONE_CHECK");
    if check {
        cmd.env("VELT_SEMA_CLONE_CHECK", "1");
    }
    let out = cmd.output().expect("velt runs");
    assert!(
        out.status.success(),
        "{golden} with the clone check {}: {}",
        if check { "on" } else { "off" },
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

#[test]
fn a_clone_of_the_checked_context_builds_the_same_program() {
    for golden in [
        "tests/golden/m1/hello.velt",
        "tests/golden/m2/classes.velt",
        "tests/golden/m2/closures.velt",
        "tests/golden/m2/generics.velt",
        "tests/golden/m3/async_basic.velt",
        "tests/golden/lang/decorators.velt",
        "tests/golden/lang/generated_functions.velt",
        "tests/golden/std/json_any_json_value.velt",
    ] {
        assert!(vir(golden, true) == vir(golden, false), "{golden}");
    }
}
