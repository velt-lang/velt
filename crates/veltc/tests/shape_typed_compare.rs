//! A typed comparison of two values of a type parameter inside an instance shared by shape
//! (velt_vir `lower/shape/`, docs/internals/design/shape-sharing.md). The shared instances are
//! the array-view helpers, so the test runs a copy of std whose `arrayViewSyncAt` also counts
//! the mirror's elements that are `===` and `Object.is` the element read, and prints the count.
//! Lowering that body with a placeholder for `T` was an ICE ("a placeholder's kind"); the
//! comparison is each type's glue, reached through the dictionary, and gives what the instance
//! of each type gives (`VELT_SHAPES=off`).

mod no_window;
mod test_dir;

use std::path::Path;

/// Views of two element types (two shapes), each read twice at one index: the second read
/// finds the element in the mirror and counts.
const VIEWS: &str = "
function twice<T>(xs: T[]): T {
  const x = xs[xs.length - 1];
  return xs[0] === x ? x : xs[0];
}
const floats: any = [0, -0, NaN, 1.5, 0];
twice(floats as f32[]);
const ints: any = [1, 2, 1, 1];
twice(ints as i32[]);
console.log(\"done\");
";

/// What `arrayViewSyncAt` does for an element the mirror has, and the same with the count.
const BEFORE: &str = "  if (i < m) {
    const cur: any = t[k];";
const AFTER: &str = "  if (i < m) {
    let same = 0;
    for (let j: usize = 0; j < m; j++) {
      if (seen[j] === seen[i]) {
        same += 1;
      }
      if (Object.is(seen[j], seen[i])) {
        same += 10;
      }
    }
    console.log(\"same\", same);
    const cur: any = t[k];";

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// `velt run` of [`VIEWS`] with the patched std in `dir` and `VELT_SHAPES=shapes`.
fn run(dir: &Path, shapes: &str) -> String {
    let mut cmd = crate::no_window::command(env!("CARGO_BIN_EXE_velt"));
    cmd.args(["run", "main.velt"])
        .current_dir(dir)
        .env("VELT_STD", dir.join("std"))
        .env("VELT_SHAPES", shapes);
    let out = cmd.output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "VELT_SHAPES={shapes}:\n{stderr}");
    String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n")
}

#[test]
fn a_typed_comparison_in_a_shared_instance_is_each_types_own() {
    let dir = test_dir::TestDir::new();
    let std = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std");
    copy_dir(&std, &dir.path().join("std"));
    let hook = dir.path().join("std/lang/hooks/array_view.velt");
    let text = std::fs::read_to_string(&hook).unwrap().replace("\r\n", "\n");
    assert_eq!(text.matches(BEFORE).count(), 1, "arrayViewSyncAt changed");
    std::fs::write(&hook, text.replace(BEFORE, AFTER)).unwrap();
    std::fs::write(dir.path().join("main.velt"), VIEWS).unwrap();
    // f32 [0, -0, NaN, 1.5, 0] at 0: three `===` (both zeros alike), two `Object.is`.
    // i32 [1, 2, 1, 1] at 0: three of each.
    let want = "same 23\nsame 33\ndone\n";
    assert_eq!(run(dir.path(), "all"), want, "shared by shape");
    assert_eq!(run(dir.path(), "off"), want, "one instance per type");
}
