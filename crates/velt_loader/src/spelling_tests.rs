//! One std file, one module name, however the path that reached it is spelled (#3731): a std test
//! file loaded as an extra root (`velt test`) through another spelling of the std root (a link, or
//! on Windows the verbatim `\\?\C:\…` form of a canonical path) imports std files by relative
//! path. Each gets its std name, so the check finds its hooks by it; before, they came out as
//! `std/../../…/std/…`, and lowering met a hook the check had not found. A std test run by `velt
//! test` doesn't show it: the runner spells the std root and the test file alike.

use std::path::{Path, PathBuf};

use super::tests::{load, messages, paths, Tree};
use super::*;

/// A std tree with a test file of `x` that imports its siblings by relative path.
fn std_tree(t: &Tree) -> PathBuf {
    t.write(
        "std/x/index.velt",
        "export { one } from \"velt/x/inner\";\n",
    );
    t.write(
        "std/x/inner.velt",
        "import { two } from \"./deeper\";\nexport function one(): i64 { return two() - 1; }\n",
    );
    t.write(
        "std/x/deeper.velt",
        "export function two(): i64 { return 2; }\n",
    );
    t.write(
        "std/x/inner.test.velt",
        "import { one } from \"./inner\";\nexport function test_one() { one(); }\n",
    );
    t.path("std")
}

/// The std module names a program gets with std at `std_root` and the test file at `test_file`.
fn names(t: &Tree, std_root: &Path, test_file: &Path) -> Vec<String> {
    let main = t.write("p/main.velt", "function main() {}\n");
    let extra = [test_file.to_path_buf()];
    let (l, d, _) = load(
        &main,
        LoadOptions {
            std_root: Some(std_root.to_path_buf()),
            extra_roots: &extra,
            ..Default::default()
        },
    );
    assert!(d.is_empty(), "{:?}", messages(&d));
    let mut names: Vec<String> = paths(&l)
        .into_iter()
        .filter(|p| p.starts_with("std/"))
        .map(String::from)
        .collect();
    names.sort();
    names
}

const WANT: [&str; 3] = ["std/x/deeper", "std/x/inner", "std/x/inner.test"];

#[test]
fn a_std_test_file_and_its_relative_imports_get_their_std_names() {
    let t = Tree::new();
    let std = std_tree(&t);
    assert_eq!(names(&t, &std, &std.join("x/inner.test.velt")), WANT);
}

/// The std root named through a link, the test file by its real path (and the other way round).
#[cfg(unix)]
#[test]
fn the_names_hold_when_a_link_reaches_std_or_the_test_file() {
    let t = Tree::new();
    let real = std_tree(&t);
    let link = t.path("linked-std");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    assert_eq!(names(&t, &link, &real.join("x/inner.test.velt")), WANT);
    assert_eq!(names(&t, &real, &link.join("x/inner.test.velt")), WANT);
}

/// The test file by its canonical, verbatim path (`\\?\C:\…`) and std by its plain one, and the
/// other way round: the two mixed spellings that hit the ICE on Windows.
#[cfg(windows)]
#[test]
fn the_names_hold_for_a_verbatim_path_on_windows() {
    let t = Tree::new();
    let std = std_tree(&t);
    let file = std.join("x/inner.test.velt");
    let verbatim_file = std::fs::canonicalize(&file).unwrap();
    let verbatim_std = std::fs::canonicalize(&std).unwrap();
    assert!(verbatim_file.to_string_lossy().starts_with(r"\\?\"));
    assert!(!std.to_string_lossy().starts_with(r"\\?\"));
    assert_eq!(names(&t, &std, &verbatim_file), WANT);
    assert_eq!(names(&t, &verbatim_std, &file), WANT);
}
