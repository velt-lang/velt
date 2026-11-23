//! Which changes run the VIR identity, and how much of it CI runs.

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
fn ci_compares_the_sample_even_when_loading_on_demand_changes() {
    // A train of ten pull requests nearly always touches one of these: CI compares the sample,
    // the nightly every golden on Linux, and the report asks the author for every golden.
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
        let p = plan(&[path]);
        assert_eq!(p.vir_identity_queue(), "sample", "{path}");
        assert!(p.vir_identity_all, "{path}");
        assert!(p.describe().contains("VELT_VIR_IDENTITY=all"), "{path}");
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
        let p = plan(&[path]);
        assert_eq!(p.vir_identity_queue(), "sample", "{path}");
        assert!(!p.vir_identity_all, "{path}");
    }
    for path in ["crates/velt_fmt/src/lib.rs", "docs/internals/README.md"] {
        assert_eq!(plan(&[path]).vir_identity_queue(), "skip", "{path}");
    }
    // CI, a lockfile or a script runs the rest of the gate, and the sample of this.
    for path in [".github/workflows/ci.yml", "Cargo.lock", "scripts/check.sh"] {
        assert_eq!(plan(&[path]).vir_identity_queue(), "sample", "{path}");
    }
    // Mixed with other files, and what can't be told (a train runs `--full`): the sample too.
    let mixed = plan(&["docs/internals/README.md", "crates/velt_loader/src/lib.rs"]);
    assert_eq!(mixed.vir_identity_queue(), "sample");
    assert_eq!(Plan::everything("--full").vir_identity_queue(), "sample");
    assert_eq!(
        Plan::unknown_changes("no base").vir_identity_queue(),
        "sample"
    );
}

/// The queue's `$GITHUB_OUTPUT` for a train that touches the std hooks: the sample, never every
/// golden (`vir_identity=` empty), which made the Windows golden shards hit their time limit.
#[test]
fn a_queue_change_to_the_std_hooks_selects_the_sample() {
    let p = plan(&["std/lang/hooks/number_text.velt", "std/json/index.velt"]);
    let text = crate::github_output_text(&p);
    assert!(text.contains("\nvir_identity=sample\n"), "{text}");
}
