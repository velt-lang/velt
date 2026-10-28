//! Standard-library code checked on demand (#1658, docs/internals/design/lazy-std.md): the free
//! functions and constants of a std module no user module imports are checked when checked code
//! reaches them; the rest stay unchecked, with stubs listed in `Program::unchecked`.

mod common;

use common::hir_walk::def_id;
use common::programs::{err_src, ok_src};
use velt_sema::hir::{Def, Program};

/// The names of the unchecked definitions of `p`.
fn unchecked(p: &Program) -> Vec<String> {
    let mut out: Vec<String> = (p.unchecked.iter())
        .map(|d| match p.def(*d) {
            Def::Fn(f) => {
                assert!(f.body.block.stmts.is_empty() && f.body.block.value.is_none());
                f.name.clone()
            }
            Def::Global(g) => g.name.clone(),
            _ => panic!("an unchecked definition is a function or a constant"),
        })
        .collect();
    out.sort();
    out
}

#[test]
fn what_no_code_reaches_stays_unchecked() {
    let p = ok_src("function main() {\n  console.log(\"hi\");\n}\n");
    let names = unchecked(&p);
    // The prelude imports `velt:datetime/format`, whose `formatPattern` only `velt:datetime`
    // calls, and hello world doesn't import that.
    for f in [
        "std/datetime/format::formatPattern",
        "std/datetime/civil::monthLong",
    ] {
        assert!(names.contains(&f.to_string()), "{f}: {names:?}");
    }
    // Modules lowering looks into by name are checked whole, and so are the prelude's helpers
    // and its types' methods.
    let eager = ["std/sort", "std/proxy", "std/typed_array"];
    for n in &names {
        assert!(n.starts_with("std/"), "{n}");
        assert!(!eager.iter().any(|e| n.starts_with(e)), "{n}");
    }
    assert!(
        !names.iter().any(|n| n.ends_with("::__anyLooseEq")),
        "a helper"
    );
    // `Number` of the prelude is a function nothing in hello world calls.
    assert!(
        names.contains(&"std/prelude/string::Number".to_string()),
        "{names:?}"
    );
}

#[test]
fn what_checked_code_calls_is_checked_with_what_it_calls() {
    // `velt:datetime`, imported, is checked whole; its methods call `formatPattern`, which calls
    // the module's private `token`, which calls `monthLong` of another lazy module.
    let p = ok_src(
        "import * as dt from \"velt:datetime\";\n\
         function main() {\n  console.log(\"hi\");\n}\n",
    );
    let names = unchecked(&p);
    for f in [
        "std/datetime/format::formatPattern",
        "std/datetime/format::token",
        "std/datetime/civil::monthLong",
    ] {
        assert!(!names.contains(&f.to_string()), "{f} is checked: {names:?}");
        assert!(matches!(p.def(def_id(&p, f)), Def::Fn(f) if !f.body.block.stmts.is_empty()));
    }
}

#[test]
fn a_module_the_program_imports_is_checked_whole() {
    let p = ok_src(
        "import { formatPattern } from \"velt:datetime/format\";\n\
         function main() {\n  console.log(\"hi\");\n}\n",
    );
    let names = unchecked(&p);
    assert!(
        !names.iter().any(|n| n.starts_with("std/datetime/format::")),
        "{names:?}"
    );
}

#[test]
fn errors_in_user_code_are_reported_as_before() {
    // The program's own unreached function is checked: lazy checking is for std only.
    let e = err_src(
        "import { parseIso } from \"velt:datetime/parse\";\n\
         function unused(): i64 { return parseIso(1); }\n\
         function main() {}\n",
    );
    assert!(e.contains("error"), "{e}");
}
