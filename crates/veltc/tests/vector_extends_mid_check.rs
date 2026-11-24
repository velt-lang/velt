//! A std module added in the middle of the check may add methods to a built-in vector type
//! (velt_loader's (a1) rule, velt/simd's `extend u8x16 { … }`, #3634), but not one the type
//! already has: a call checked before it would change (velt_sema `collect::vector_extends`). On a
//! copy of std whose on-demand integer power hook (std/lang/hooks/pow_int.velt, demanded by `**`
//! on integers) gets an `extend i8x16` block: with a new method the program builds with one
//! check; with velt/simd's own `sum` it is an internal error.

use std::path::Path;

mod no_window;
mod test_dir;

const VELT: &str = env!("CARGO_BIN_EXE_velt");

const PROGRAM: &str = r#"import { i8x16 } from "velt/simd";

function main() {
  const v = i8x16.splat(1);
  console.log(v.sum());
  let n: i64 = 3;
  console.log(n ** 2);
}
"#;

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        let dest = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&path, &dest);
        } else {
            std::fs::copy(&path, &dest).unwrap();
        }
    }
}

/// `velt build --timings` of the program against std at `std`: its success, stderr, and how many
/// checks it took (a `sema` line each).
fn build(dir: &Path, std: &Path) -> (bool, String, usize) {
    let o = no_window::command(VELT)
        .args(["build", "main.velt", "-o"])
        .arg(dir.join("main.bin"))
        .arg("--timings")
        .current_dir(dir)
        .env("VELT_STD", std)
        .env("VELT_BUILD_CACHE", "0")
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&o.stderr).into_owned();
    let checks = err.lines().filter(|l| l.starts_with("velt: sema ")).count();
    (o.status.success(), err, checks)
}

#[test]
fn a_mid_check_vector_method_may_not_shadow_one() {
    let dir = test_dir::TestDir::new();
    let root = dir.path();
    std::fs::write(root.join("main.velt"), PROGRAM).unwrap();
    let real = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std");
    let std = root.join("std");
    copy_dir(&real, &std);
    let hook = std.join("lang/hooks/pow_int.velt");
    let original = std::fs::read_to_string(&hook).unwrap();

    // A method i8x16 doesn't have yet: served in place, one check.
    std::fs::write(
        &hook,
        format!("{original}\nextend i8x16 {{\n  zzNotInSimd(): i8 {{\n    return 0;\n  }}\n}}\n"),
    )
    .unwrap();
    let (ok, err, checks) = build(root, &std);
    assert!(ok, "{err}");
    assert_eq!(checks, 1, "{err}");

    // velt/simd's own `sum`: the program's `v.sum()` was checked against it before.
    std::fs::write(
        &hook,
        format!("{original}\nextend i8x16 {{\n  sum(): i8 {{\n    return 0;\n  }}\n}}\n"),
    )
    .unwrap();
    let (ok, err, _) = build(root, &std);
    assert!(!ok, "a shadowing method was accepted");
    assert!(
        err.contains("ICE: a std module added during the check gives `i8x16` a method `sum`"),
        "{err}"
    );
}
