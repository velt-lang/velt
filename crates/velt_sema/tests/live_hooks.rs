//! The hooks sema demands for lowering (#2161 step 2, velt_common `hook_demand.rs`): each hook
//! whose table entry lists constructs is demanded, and checked, only where *live* code has one of
//! them (user code, and the std code it reaches, std's own conversions included); the others
//! always are. Lowering asserts that a hook it calls was demanded (`velt_vir` `hook_calls.rs`).

mod common;

use common::programs::ok_src;
use velt_common::std_hooks::{hook_index, HOOKS};
use velt_sema::hir::{Def, Program};

const INT: [usize; 6] = [
    hook_index::int_text,
    hook_index::uint_text,
    hook_index::int_push,
    hook_index::uint_push,
    hook_index::int_write,
    hook_index::uint_write,
];

const INT_NAMES: [&str; 6] = [
    "int_text",
    "uint_text",
    "int_push",
    "uint_push",
    "int_write",
    "uint_write",
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

/// #2513: the type alone brings none of the integer hooks' code into the program.
#[test]
fn a_struct_or_class_nothing_prints_demands_no_integer_hook() {
    for src in [
        "struct P { v: u32; }\n\
         function main() {\n  const p: P = { v: 1 };\n  console.log(p.v > 0);\n}\n",
        "class P { v: u32 = 1; }\n\
         function main() {\n  const p = new P();\n  console.log(p.v > 0);\n}\n",
    ] {
        let p = ok_src(src);
        assert_eq!(demanded(&p), Vec::<&str>::new(), "{src}");
        for i in INT {
            assert!(!checked(&p, i), "{} is checked:\n{src}", HOOKS[i].name);
        }
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
        // Format: `console.log` of an unsigned integer.
        ("  const n: u64 = 7;\n  console.log(n);", &["uint_write"]),
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
fn a_class_converted_to_any_demands_its_format_slot() {
    let p = ok_src("class P {\n  x = 1;\n}\nfunction main() {\n  const a: any = new P();\n  if (a == 2) {\n    return;\n  }\n}\n");
    let got = demanded(&p);
    for w in ["int_push", "uint_push"] {
        assert!(got.contains(&w), "{w} not demanded: {got:?}");
    }
}

/// The integer hooks act as one bit, which is what makes the demand sound (velt_common
/// `hook_demand.rs`): any one construct in a program demands and checks all of them,
/// `console.log`'s integer writers included, through the std code of the first. If this fails,
/// live code must first cover what lowering builds (#2161).
#[test]
fn any_one_construct_demands_and_checks_all_the_integer_hooks() {
    for src in [
        // TextInt, alone in the program's code.
        "function main() {\n  const n: i64 = 7;\n  const s = `n=${n}`;\n}\n",
        // TextUint.
        "function main() {\n  const n: u32 = 7;\n  const s = String(n);\n}\n",
        // Format: `console.log` of an integer.
        "function main() {\n  const n: i32 = 7;\n  console.log(n);\n}\n",
        // IndexCheck: the out-of-bounds message of an index.
        "function main() {\n  const xs = [\"a\"];\n  const s = xs[0];\n}\n",
        // Format: printing a class with a number in it.
        "class P {\n  x = 1;\n}\nfunction main() {\n  console.log(new P());\n}\n",
        // Dynamic: the numeric keys of an `any`.
        "function main() {\n  const a: any = \"s\";\n}\n",
    ] {
        let p = ok_src(src);
        assert_eq!(demanded(&p), INT_NAMES, "{src}");
        for i in INT {
            assert!(checked(&p, i), "{} is not checked:\n{src}", HOOKS[i].name);
        }
    }
}

/// `console.log` of a float checks its writer (`numberWrite`, velt_common `ON_DEMAND_WRITE`) and
/// the integer writer it holds, which only its whole numbers reach, but demands no integer hook
/// and checks none of the text hooks (their module, velt/lang/hooks/number_text, is loaded for the
/// integer writer). A program that prints no float leaves the float writer unchecked (`velt_lsp`'s
/// `demand` test: nor does it load its module).
#[test]
fn a_float_printed_checks_its_writer_and_no_text_hook() {
    let number_write = hook_index::number_write;
    for body in [
        "  const x: f64 = 0.5;\n  console.log(x);",
        "  const x: f32 = 0.5;\n  console.error(\"x\", x);",
    ] {
        let p = main_of(body);
        assert_eq!(demanded(&p), Vec::<&str>::new(), "{body}");
        assert!(
            checked(&p, number_write),
            "numberWrite is not checked:\n{body}"
        );
        assert!(
            checked(&p, hook_index::int_write),
            "intWrite is not checked:\n{body}"
        );
        for i in [
            hook_index::int_text,
            hook_index::uint_text,
            hook_index::int_push,
        ] {
            assert!(!checked(&p, i), "{} is checked:\n{body}", HOOKS[i].name);
        }
    }
    let p = main_of("  const n: i64 = 7;\n  console.log(n);");
    assert!(
        !checked(&p, number_write),
        "numberWrite is checked for an integer"
    );
    // A type parameter's value may be a float.
    let p = ok_src(
        "function show<T>(x: T) {\n  console.log(x);\n}\nfunction main() {\n  show(0.5);\n}\n",
    );
    assert!(
        checked(&p, number_write),
        "numberWrite is not checked for a `T`"
    );
}
