//! The hooks sema demands for lowering (#2161 step 2, velt_common `hook_demand.rs`): each hook
//! whose table entry lists constructs is demanded, and checked, only where *live* code has one of
//! them (user code, and the std code it reaches, std's own conversions included); the others
//! always are. Lowering asserts that a hook it calls was demanded (`velt_vir` `hook_calls.rs`).

mod common;

use common::programs::ok_src;
use velt_common::std_hooks::{hook_index, HOOKS};
use velt_sema::hir::{Def, Program};

const INT: [usize; 7] = [
    hook_index::int_text,
    hook_index::uint_text,
    hook_index::int_push,
    hook_index::uint_push,
    hook_index::int_write,
    hook_index::uint_write,
    hook_index::number_write,
];

const INT_NAMES: [&str; 7] = [
    "int_text",
    "uint_text",
    "int_push",
    "uint_push",
    "int_write",
    "uint_write",
    "number_write",
];

fn main_of(body: &str) -> Program {
    ok_src(&format!("function main() {{\n{body}\n}}\n"))
}

/// The integer hooks `p` demands, by entry name.
fn demanded(p: &Program) -> Vec<&'static str> {
    (INT.iter())
        .filter(|&&i| p.hooks_demanded.contains(i))
        .map(|&i| HOOKS[i].name)
        .collect()
}

/// Is hook `i` checked in `p` (a body, not a stub in `unchecked`)?
fn checked(p: &Program, i: usize) -> bool {
    let q = HOOKS[i].qualified();
    (p.defs.iter().enumerate()).any(|(d, f)| {
        matches!(f, Def::Fn(f) if f.name == q) && !p.unchecked.iter().any(|u| u.0 as usize == d)
    })
}

#[test]
fn a_program_that_prints_no_integer_demands_no_integer_hook() {
    let p = main_of("  console.log(\"hi\");");
    assert_eq!(demanded(&p), Vec::<&str>::new());
    for i in INT {
        assert!(!checked(&p, i), "{} is checked", HOOKS[i].name);
    }
    // A hook no construct lists is always demanded.
    for (i, h) in HOOKS.iter().enumerate() {
        assert_eq!(
            p.hooks_demanded.contains(i),
            h.constructs.is_empty(),
            "{}",
            h.name
        );
    }
}

#[test]
fn each_construct_demands_its_hooks() {
    for (body, want) in [
        // TextInt: a template's `${n}`, appended or alone.
        ("  const n: i64 = 7;\n  console.log(`n=${n}`);", &["int_text", "int_push"][..]),
        // TextUint.
        ("  const n: u32 = 7;\n  console.log(String(n));", &["uint_text", "uint_push"]),
        // Format: `console.log` of an integer.
        ("  const n: i32 = 7;\n  console.log(n);", &["int_push", "uint_push", "int_write"]),
        // Format: `console.log` of an unsigned integer, of a float.
        ("  const n: u64 = 7;\n  console.log(n);", &["uint_write"]),
        ("  const x: f64 = 0.5;\n  console.log(x);", &["number_write", "int_write"]),
        ("  const x: f32 = 0.5;\n  console.error(x);", &["number_write", "int_write"]),
        // IndexCheck: the out-of-bounds message of an index.
        ("  const xs = [\"a\"];\n  const i: i64 = 0;\n  const s = xs[i];\n  if (s == \"b\") {\n    return;\n  }", &["int_text", "uint_text"]),
        // Dynamic: numeric keys of an `any`.
        ("  const a: any = \"s\";\n  if (a == \"t\") {\n    return;\n  }", &["uint_text", "int_push", "uint_push"]),
    ] {
        let p = main_of(body);
        let got = demanded(&p);
        for w in want {
            assert!(got.contains(w), "{w} not demanded by\n{body}\n(demanded: {got:?})");
            let i = INT[INT_NAMES.iter().position(|n| n == w).unwrap()];
            assert!(checked(&p, i), "{w} demanded but not checked:\n{body}");
        }
    }
}

#[test]
fn a_conversion_in_std_code_that_live_code_reaches_counts() {
    // `string.repeat` reports a bad count with `${n}` of an `i64`: std's code, which the
    // program's call makes live; the program's own code converts nothing.
    let p = main_of("  console.log(\"ab\".repeat(3));");
    let got = demanded(&p);
    assert!(
        got.contains(&"int_text") && got.contains(&"int_push"),
        "{got:?}"
    );
    // The same method unreached: std's conversion is not live.
    let p = main_of("  console.log(\"ab\");");
    assert_eq!(demanded(&p), Vec::<&str>::new());
}

#[test]
fn a_hook_lowering_calls_where_sema_checked_it_is_live() {
    // A number printed through the float hooks, checked on demand: std's float printer
    // (velt/lang/number/format) counts digits with a table, whose index checks lowering builds
    // too.
    let p = main_of("  const x: number = 7.5;\n  console.log(`x=${x}`);");
    let got = demanded(&p);
    for w in ["int_text", "uint_text"] {
        assert!(got.contains(&w), "{w} not demanded: {got:?}");
    }
}

#[test]
fn an_integer_thrown_and_not_caught_demands_its_printing() {
    for src in [
        "function main() { throw 5 as i32; }\n",
        "async function main() { throw 5 as i32; }\n",
    ] {
        let got = demanded(&ok_src(src));
        assert!(got.contains(&"int_push"), "{src}: {got:?}");
    }
}

#[test]
fn a_class_with_a_number_in_it_demands_its_format_slot() {
    let p = ok_src("class P {\n  x = 1;\n}\nfunction main() {\n  const p = new P();\n  if (p.x == 2) {\n    return;\n  }\n}\n");
    let got = demanded(&p);
    for w in ["int_push", "uint_push"] {
        assert!(got.contains(&w), "{w} not demanded: {got:?}");
    }
}

/// Every integer hook a program demands is checked, whichever construct demanded it, through the
/// std code of the first (velt_common `hook_demand.rs`). If this fails, live code must first
/// cover what lowering builds (#2161).
#[test]
fn any_one_construct_checks_the_integer_hooks_it_demands() {
    for src in [
        // TextInt, alone in the program's code.
        "function main() {\n  const n: i64 = 7;\n  const s = `n=${n}`;\n}\n",
        // TextUint.
        "function main() {\n  const n: u32 = 7;\n  const s = String(n);\n}\n",
        // Format: `console.log` of an integer.
        "function main() {\n  const n: i32 = 7;\n  console.log(n);\n}\n",
        // IndexCheck: the out-of-bounds message of an index.
        "function main() {\n  const xs = [\"a\"];\n  const s = xs[0];\n}\n",
        // Format: the table of a class with a number in it (its `format` slot).
        "class P {\n  x = 1;\n}\nfunction main() {\n  const p = new P();\n}\n",
        // Dynamic: the numeric keys of an `any`.
        "function main() {\n  const a: any = \"s\";\n}\n",
        // Format: `console.log` of a float, in a program with no integer of its own.
        "function main() {\n  const x: f64 = 0.5;\n  console.log(x);\n}\n",
    ] {
        let p = ok_src(src);
        let got = demanded(&p);
        assert!(!got.is_empty(), "nothing demanded:\n{src}");
        for (i, name) in INT.iter().zip(INT_NAMES) {
            if got.contains(&name) {
                assert!(checked(&p, *i), "{name} is not checked:\n{src}");
            }
        }
    }
}

/// std's integer printer reads no table (velt/lang/number/integer), so printing an integer has no
/// index check whose message would demand the text hooks: `console.log(n)` doesn't build
/// `uintText`. An index in the program's own code still demands both.
#[test]
fn printing_an_integer_does_not_demand_the_text_hooks() {
    let p = ok_src("function main() {\n  const n: i32 = 7;\n  console.log(n);\n}\n");
    let got = demanded(&p);
    assert!(got.contains(&"int_write"), "{got:?}");
    for w in ["int_text", "uint_text"] {
        assert!(!got.contains(&w), "{w} demanded: {got:?}");
    }
    let p = ok_src("function main() {\n  const xs = [\"a\"];\n  const s = xs[0];\n}\n");
    let got = demanded(&p);
    for w in ["int_text", "uint_text"] {
        assert!(got.contains(&w), "{w} not demanded: {got:?}");
    }
}
