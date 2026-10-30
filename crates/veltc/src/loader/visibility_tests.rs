//! Which std files a module may load ([`super::visibility`]).

use super::tests::{load, messages, paths, Tree};
use super::*;

/// A std root with the module `velt/x` (`x/index.vlt`), its internal file `x/inner.vlt` and a
/// white-box test of it next to them.
fn std_with_a_test(t: &Tree) -> PathBuf {
    t.write("std/x/index.vlt", "export { one } from \"velt/x/inner\";\n");
    t.write(
        "std/x/inner.vlt",
        "export function one(): i64 { return 1; }\n",
    );
    t.write(
        "std/x/inner.test.vlt",
        "import { one } from \"velt/x/inner\";\nexport function test_one() { one(); }\n",
    );
    t.path("std")
}

#[test]
fn std_test_files_are_std_code_wherever_they_are_imported_from() {
    // `velt test std/`: its generated harness, a virtual file in std/, imports the test files
    // relatively, and they import the internals they test.
    let t = Tree::new();
    let std = std_with_a_test(&t);
    let harness = std.join("harness.vlt");
    let (l, d, _) = load(
        &harness,
        LoadOptions {
            std_root: Some(std.clone()),
            root_source: Some(
                "import * as t from \"./x/inner.test.vlt\";\nfunction main() { t.test_one(); }\n"
                    .into(),
            ),
            ..Default::default()
        },
    );
    assert!(d.is_empty(), "{:?}", messages(&d));
    assert_eq!(paths(&l), ["main", "std/x/inner.test", "std/x/inner"]);
    assert!(l.modules[1].is_std && l.modules[2].is_std);

    // Any other std file reached that way is still an error.
    let (_, d, _) = load(
        &harness,
        LoadOptions {
            std_root: Some(std),
            root_source: Some("import { one } from \"./x/inner\";\nfunction main() {}\n".into()),
            ..Default::default()
        },
    );
    assert_eq!(
        messages(&d),
        ["module `./x/inner` is a file of the standard library: import it as `velt/x/inner`"]
    );
}
