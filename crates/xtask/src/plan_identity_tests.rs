//! Which changes run the VIR identity, and how much of it the merge queue runs.

use super::*;

#[test]
fn the_vir_identity_runs_when_sema_the_loader_lowering_or_std_change() {
    for path in [
        "crates/velt_sema/src/lib.rs",
        "crates/velt_loader/src/lib.rs",
        "crates/velt_vir/src/lib.rs",
        "crates/velt_syntax/src/lib.rs",
        "crates/velt_common/src/lib.rs",
        "crates/vpm/src/lib.rs",
        "crates/velt_build_cache/src/lib.rs",
        "crates/veltc/src/driver.rs",
        "crates/veltc/src/driver/recover.rs",
        "crates/veltc/src/driver/std_load.rs",
        "crates/veltc/tests/eager_std_identity.rs",
        "std/lang/hooks/number_text.velt",
        "std/json/index.velt",
    ] {
        assert!(plan(&[path]).vir_identity, "{path}");
    }
    for path in ["crates/velt_fmt/src/lib.rs", "docs/internals/README.md"] {
        assert!(!plan(&[path]).vir_identity, "{path}");
    }
}

#[test]
fn the_queue_compares_every_golden_only_when_loading_on_demand_changes() {
    for path in [
        "crates/velt_loader/src/more.rs",
        "crates/velt_sema/src/body/lazy_std/live.rs",
        "crates/velt_common/src/std_hooks.rs",
        "crates/veltc/src/driver.rs",
        "crates/veltc/src/driver/recover.rs",
        "crates/veltc/src/driver/std_load.rs",
        "crates/veltc/tests/eager_std_identity.rs",
        "std/lang/hooks/number_text.velt",
    ] {
        assert_eq!(plan(&[path]).vir_identity_queue(), "", "{path}");
    }
    for path in [
        "crates/velt_sema/src/lib.rs",
        "crates/velt_vir/src/lib.rs",
        "crates/velt_syntax/src/lib.rs",
        "crates/velt_common/src/lib.rs",
        "crates/vpm/src/lib.rs",
        "crates/velt_build_cache/src/lib.rs",
        "std/json/index.velt",
    ] {
        assert_eq!(plan(&[path]).vir_identity_queue(), "sample", "{path}");
    }
    for path in ["crates/velt_fmt/src/lib.rs", "docs/internals/README.md"] {
        assert_eq!(plan(&[path]).vir_identity_queue(), "skip", "{path}");
    }
    // CI, a lockfile or a script runs the rest of the gate, and the sample of this.
    for path in [".github/workflows/ci.yml", "Cargo.lock", "scripts/check.sh"] {
        assert_eq!(plan(&[path]).vir_identity_queue(), "sample", "{path}");
    }
    // One such file among others is enough, and what can't be told runs it all.
    let mixed = plan(&["docs/internals/README.md", "crates/velt_loader/src/lib.rs"]);
    assert_eq!(mixed.vir_identity_queue(), "");
    assert_eq!(Plan::everything("--full").vir_identity_queue(), "");
    assert_eq!(Plan::unknown_changes("no base").vir_identity_queue(), "");
}
