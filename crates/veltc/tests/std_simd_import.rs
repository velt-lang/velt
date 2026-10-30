//! A std module may use the SIMD type names without importing them (they are built in there),
//! but their methods come from `velt/simd`: one that calls `u8x16.splat` without the import
//! gets an error that names the import, not only "not a static method". std is copied to a
//! temp directory with a probe module added, and `velt check` runs with `$VELT_STD` on it.

use std::path::Path;

mod no_window;
mod test_dir;

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            copy_tree(&path, &to.join(entry.file_name()));
        } else {
            std::fs::copy(&path, to.join(entry.file_name())).unwrap();
        }
    }
}

#[test]
fn std_module_without_the_import_is_told_to_import() {
    let dir = test_dir::TestDir::new();
    let std = dir.path().join("std");
    copy_tree(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std"),
        &std,
    );
    std::fs::write(
        std.join("zz_probe.vlt"),
        "export function probe(): u8 {\n  return u8x16.splat(3).lane(0);\n}\n",
    )
    .unwrap();
    let main = dir.path().join("main.vlt");
    std::fs::write(
        &main,
        "import { probe } from \"velt:zz_probe\";\n\nfunction main() {\n  console.log(probe());\n}\n",
    )
    .unwrap();
    let out = no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["check", main.to_str().unwrap()])
        .env("VELT_STD", &std)
        .current_dir(dir.path())
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{err}");
    assert!(
        err.contains("`u8x16.splat` is not a static method"),
        "{err}"
    );
    assert!(err.contains("import { u8x16 } from \"velt/simd\""), "{err}");
}
