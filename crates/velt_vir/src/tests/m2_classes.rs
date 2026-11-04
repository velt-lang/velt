//! `tests/golden/m2/classes.vlt` (class objects, constructors with field defaults, `super(...)`,
//! virtual dispatch through vtables, `Animal[]` holding a `Dog`, moves and `clone()` of objects)
//! and printing objects and arrays: their numbers are printed through std's hooks (#2197), so
//! the programs are loaded from source with the real std (std_src.rs).

use super::{m2_golden, run_src};

#[test]
fn golden_classes() {
    let out = run_src(&super::golden_src("m2", "classes"));
    assert_eq!(out.stdout, m2_golden("classes"));
    assert_eq!(out.code, 0);
}

#[test]
fn print_objects_and_arrays() {
    let src = "struct Point {\n  x: f64;\n  name: string;\n}\n\nfunction main() {\n  \
               const ints: i64[] = [1, 2, 3];\n  console.log(ints);\n  const none: i64[] = [];\n  \
               console.log(none, [\"a\", \"b\"]);\n  const nested: i64[][] = [[1], []];\n  \
               console.log(nested);\n  const n: i64 = 7;\n  \
               console.log(Point { x: 1.5, name: \"p\" }, { n: n });\n  const no: string | null = null;\n  \
               const yes: string | null = \"x\";\n  console.log(no, yes);\n}\n";
    let out = run_src(src);
    assert_eq!(
        out.stdout,
        "[ 1, 2, 3 ]\n[] [ 'a', 'b' ]\n[ [ 1 ], [] ]\nPoint { x: 1.5, name: 'p' } { n: 7 }\nnull x\n"
    );
}
