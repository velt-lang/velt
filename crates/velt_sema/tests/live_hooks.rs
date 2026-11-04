//! The hooks sema demands for lowering (#2161 step 2, velt_common `hook_demand.rs`): each hook
//! whose table entry lists constructs is demanded, and checked, only where *live* code has one of
//! them (user code, and the std code it reaches, std's own conversions included); the others
//! always are. Lowering asserts that a hook it calls was demanded (`velt_vir` `hook_calls.rs`).

mod common;

use common::programs::ok_src;
use velt_common::std_hooks::{hook_index, HOOKS};
use velt_sema::hir::{Def, Program};

const INT: [usize; 4] = [
    hook_index::int_text,
    hook_index::uint_text,
    hook_index::int_push,
    hook_index::uint_push,
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
        ("  const n: i32 = 7;\n  console.log(n);", &["int_push", "uint_push"]),
        // IndexCheck: the out-of-bounds message of an index.
        ("  const xs = [\"a\"];\n  const i: i64 = 0;\n  const s = xs[i];\n  if (s == \"b\") {\n    return;\n  }", &["int_text", "uint_text"]),
        // Dynamic: numeric keys of an `any`.
        ("  const a: any = \"s\";\n  if (a == \"t\") {\n    return;\n  }", &["uint_text", "int_push", "uint_push"]),
    ] {
        let p = main_of(body);
        let got = demanded(&p);
        for w in want {
            assert!(got.contains(w), "{w} not demanded by\n{body}\n(demanded: {got:?})");
            let i = INT[["int_text", "uint_text", "int_push", "uint_push"]
                .iter()
                .position(|n| n == w)
                .unwrap()];
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
    // A number printed through the float hooks, checked on demand (`numberPush`): its whole
    // numbers take the integer printing (`intPush`, called by name in std's code), whose
    // index checks lowering builds too.
    let p = main_of("  const x: number = 7;\n  console.log(`x=${x}`);");
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
