//! `VELT_LIVE_CHECK` works the live set out in full for lowering to check against (velt_sema
//! `body/lazy_std/live.rs`) and changes nothing else: a compiler with it on, and one with it off,
//! as a release compiler is by default, give the same bytes. Off, the walk stops once every hook
//! is demanded, so this also says that going on demands nothing more.

mod no_window;
mod test_dir;

use std::path::Path;

fn vir(golden: &str, check: &str) -> Vec<u8> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    build(&root, &[golden], check)
}

/// `velt build <args> --emit vir` in `dir`: the VIR.
fn build(dir: &Path, args: &[&str], check: &str) -> Vec<u8> {
    let home = test_dir::TestDir::new();
    let out = no_window::command(env!("CARGO_BIN_EXE_velt"))
        .current_dir(dir)
        .arg("build")
        .args(args)
        .args(["--emit", "vir"])
        .env("VELT_LIVE_CHECK", check)
        .env("VELT_HOME", home.path())
        .env("VELT_BUILD_CACHE", "off")
        .env("VELT_BUILD_STAMP", "off")
        .output()
        .expect("velt runs");
    assert!(
        out.status.success(),
        "{args:?} with VELT_LIVE_CHECK={check}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

#[test]
fn the_check_changes_no_vir() {
    for golden in [
        "tests/golden/m1/hello.velt",
        "tests/golden/lang/any_array_view_args.velt",
        "tests/golden/lang/any_call_order.velt",
        "tests/golden/lang/generator_iface_to_thread_panics.velt",
        "tests/golden/lang/iface_any_args_optional_array_field.velt",
        "tests/golden/lang/hook_shapes.velt",
        "tests/golden/lang/interface_upcast_write.velt",
    ] {
        assert!(vir(golden, "0") == vir(golden, "1"), "{golden}");
    }
}

/// No golden is built for a host target: lowering awaits this call through `hostAwait`, which only
/// the rule for `HostSettled` makes live.
#[test]
fn an_async_host_function_lowers_with_the_check_on() {
    let dir = test_dir::TestDir::new();
    std::fs::write(
        dir.path().join("main.velt"),
        "declare host(\"env\") async function f(): Promise<i64>;\n\
         function main(): void {\n  spawn(f());\n  console.log(\"ok\");\n}\n",
    )
    .expect("write");
    let args = ["main.velt", "--target", "wasm32-unknown-unknown"];
    assert!(build(dir.path(), &args, "1") == build(dir.path(), &args, "0"));
}
