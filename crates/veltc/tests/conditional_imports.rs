//! Conditional imports (`#name` specifiers, `imports` in package.velt) through the `velt` binary:
//! `velt check` compares the variants of each `#name` and checks only the variant its target
//! selects as code of the build; `velt check --target` checks another target's. (Which variant a
//! build runs: the golden tests/golden/lang/conditional_imports, natively and in
//! wasm_goldens.rs.)

use std::path::Path;
use std::process::Output;

mod no_window;
mod test_dir;

const MANIFEST: &str = r##"import type { Package } from "velt/package";

export const pkg: Package = {
  name: "app",
  version: "0.1.0",
  imports: {
    "#io": { js: "./src/io/js.velt", default: "./src/io/native.velt" },
  },
};
"##;

fn write(dir: &Path, rel: &str, src: &str) {
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, src).unwrap();
}

fn velt(dir: &Path, args: &[&str]) -> Output {
    velt_with_home(dir, &dir.join(".home"), args)
}

fn velt_with_home(dir: &Path, home: &Path, args: &[&str]) -> Output {
    no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(args)
        .current_dir(dir)
        .env("VELT_HOME", home)
        .env_remove("VELT_REGISTRY")
        .env_remove("VELT_LAUNCHER")
        .env("NO_COLOR", "1")
        .output()
        .unwrap()
}

/// A package whose `#io` has a native and a `js` variant with the same API.
fn package(dir: &Path) {
    write(dir, "package.velt", MANIFEST);
    write(
        dir,
        "velt.lock.json",
        "{\n  \"version\": 1,\n  \"packages\": []\n}\n",
    );
    write(
        dir,
        "src/main.velt",
        "import { read } from \"#io\";\n\nconsole.log(read(1));\n",
    );
    write(
        dir,
        "src/io/native.velt",
        "export function read(n: i64): string {\n  return `native ${n}`;\n}\n",
    );
    write(
        dir,
        "src/io/js.velt",
        "export function read(n: i64): string {\n  return `js ${n}`;\n}\n",
    );
}

#[test]
fn check_compares_the_variants() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    package(dir);
    let o = velt(dir, &["check"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    write(
        dir,
        "src/io/js.velt",
        "export function read(n: f64): string {\n  return `js ${n}`;\n}\n",
    );
    let o = velt(dir, &["check"]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(!o.status.success(), "{err}");
    assert!(
        err.contains("`#io`: `read` has another signature in `./src/io/native.velt` than in `./src/io/js.velt`"),
        "{err}"
    );
}

#[test]
fn check_checks_the_variant_of_its_target() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    package(dir);
    // A type error in the `js` variant only: a native check does not compile it.
    write(
        dir,
        "src/io/js.velt",
        "export function read(n: i64): string {\n  return n;\n}\n",
    );
    let o = velt(dir, &["check"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let o = velt(dir, &["check", "--target", "wasm32-unknown-unknown"]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(!o.status.success(), "{err}");
    // The location is a path as the system writes it: `src\io\js.velt` on Windows.
    let path = Path::new("src").join("io").join("js.velt");
    assert!(err.contains(&format!("{}:2", path.display())), "{err}");
}

#[test]
fn check_rejects_a_target_it_does_not_build_for() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    package(dir);
    for args in [
        &["check", "--target", "bogus-triple"][..],
        &["check", "--std", "--target", "riscv64gc-unknown-linux-gnu"],
        &["check", "--target", "aarch64-apple-ios"],
        &["check", "--target", "aarch64-linux-android"],
    ] {
        let o = velt(dir, args);
        let err = String::from_utf8_lossy(&o.stderr);
        assert!(!o.status.success(), "{args:?}: {err}");
        assert!(err.contains("unsupported target"), "{args:?}: {err}");
    }
    // The golden fixture checks as a package.
    let golden =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/golden/lang/conditional_imports");
    let o = velt_with_home(&golden, &dir.join(".home"), &["check"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
}
