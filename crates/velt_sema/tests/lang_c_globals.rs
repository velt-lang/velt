//! C variables (`declare c("lib") let name: T;`, docs/internals/design/c-ffi.md "C variables"):
//! what they may be, that reads and writes need unsafe code, and that a `const` one is only read.

mod common;

use common::programs::{err_src, ok_src};
use velt_sema::hir::{self, CTy, IntTy};

const HEAD: &str = "import { Ptr } from \"velt/ffi\";\n\
declare c type FILE;\n\
declare c(\"c\") let environ: Ptr<Ptr<u8> | null>;\n\
declare c(\"c\") let verbosity: i32;\n\
declare c(\"c\") const stdout_file: Ptr<FILE> | null;\n\
declare c(\"c\") let ready: bool;\n";

fn ok(rest: &str) -> hir::Program {
    ok_src(&format!("{HEAD}{rest}\n"))
}

fn err(rest: &str) -> String {
    err_src(&format!("{HEAD}{rest}\n"))
}

/// The C variables of `p`, by symbol.
fn globals(p: &hir::Program) -> Vec<hir::CGlobal> {
    let mut out: Vec<hir::CGlobal> = (p.defs.iter())
        .filter_map(|d| match d {
            hir::Def::Global(g) => g.c_global.clone(),
            _ => None,
        })
        .collect();
    out.sort_by(|a, b| a.symbol.cmp(&b.symbol));
    out
}

#[test]
fn variables_used_in_unsafe_code_are_recorded_with_what_crosses() {
    let p = ok("function main() {
           unsafe {
             const e = environ;
             verbosity = verbosity + 1;
             verbosity += 2;
             ready = !ready;
             console.log(stdout_file === null, Ptr.addressOf(e));
           }
         }");
    let got: Vec<(String, String, CTy, bool)> = (globals(&p).into_iter())
        .map(|g| (g.symbol, g.library, g.ty, g.writable))
        .collect();
    let c = |s: &str| s.to_string();
    assert_eq!(
        got,
        [
            (c("environ"), c("c"), CTy::Ptr { nullable: false }, true),
            (c("ready"), c("c"), CTy::Bool, true),
            (c("stdout_file"), c("c"), CTy::Ptr { nullable: true }, false),
            (c("verbosity"), c("c"), CTy::Int(IntTy::I32), true),
        ]
    );
}

#[test]
fn an_unsafe_function_may_use_them_and_a_closure_in_unsafe_code_too() {
    ok(
        "unsafe function bump(): i32 { verbosity += 1; return verbosity; }
        function main() {
          const f = unsafe { () => verbosity };
          unsafe { console.log(bump(), f()); }
        }",
    );
}

#[test]
fn a_read_outside_unsafe_code_is_an_error() {
    let e = err("function main() { console.log(verbosity); }");
    assert!(
        e.contains("using C variable `verbosity` needs an `unsafe` block or function"),
        "{e}"
    );
    assert!(e.contains("// SAFETY:"), "{e}");
}

#[test]
fn a_write_outside_unsafe_code_is_an_error() {
    let e = err("function main() { verbosity = 3; }");
    assert!(
        e.contains("using C variable `verbosity` needs an `unsafe` block or function"),
        "{e}"
    );
}

#[test]
fn a_const_variable_is_not_assigned() {
    let e = err("function main() { unsafe { stdout_file = null; } }");
    assert!(
        e.contains("cannot assign to C variable `stdout_file`"),
        "{e}"
    );
    assert!(e.contains("declared `const` is only read"), "{e}");
}

#[test]
fn a_type_that_does_not_cross_is_an_error() {
    let e = err_src(
        "declare c(\"c\") let name: string;\n\
         function main() { unsafe { console.log(name); } }\n",
    );
    assert!(
        e.contains("C variable `name` has type `string`, which can't hold a C variable"),
        "{e}"
    );
}

/// Declared and not used, a C variable is still checked and recorded (the header check sees it),
/// though nothing links it.
#[test]
fn an_unused_variable_is_recorded_too() {
    let p = ok("function main() {}");
    assert_eq!(globals(&p).len(), 4);
}
