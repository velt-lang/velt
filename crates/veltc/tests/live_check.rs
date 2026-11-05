//! `VELT_LIVE_CHECK` works the live set out in full for lowering to check against (velt_sema
//! `body/lazy_std/live.rs`) and changes nothing else: a compiler with it on, and one with it off,
//! as a release compiler is by default, give the same bytes. Off, the walk stops once every hook
//! is demanded, so this also says that going on demands nothing more.

mod no_window;
mod test_dir;

use std::path::Path;

fn vir(golden: &str, check: &str) -> Vec<u8> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let home = test_dir::TestDir::new();
    let out = no_window::command(env!("CARGO_BIN_EXE_velt"))
        .current_dir(&root)
        .args(["build", golden, "--emit", "vir"])
        .env("VELT_LIVE_CHECK", check)
        .env("VELT_HOME", home.path())
        .env("VELT_BUILD_CACHE", "off")
        .env("VELT_BUILD_STAMP", "off")
        .output()
        .expect("velt runs");
    assert!(
        out.status.success(),
        "{golden} with VELT_LIVE_CHECK={check}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

#[test]
fn the_check_changes_no_vir() {
    for golden in [
        "tests/golden/m1/hello.vlt",
        "tests/golden/lang/any_array_view_args.vlt",
        "tests/golden/lang/any_call_order.vlt",
        "tests/golden/lang/generator_iface_to_thread_panics.vlt",
        "tests/golden/lang/iface_any_args_optional_array_field.vlt",
        "tests/golden/lang/hook_shapes.vlt",
        "tests/golden/lang/interface_upcast_write.vlt",
    ] {
        assert!(vir(golden, "0") == vir(golden, "1"), "{golden}");
    }
}
