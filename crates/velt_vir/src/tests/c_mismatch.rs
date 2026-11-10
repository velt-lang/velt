//! C functions whose calls don't fit: a variadic declaration and a plain one of one symbol (the
//! program's and std's system call), and a call lowering meets without its fixed arguments (sema
//! reported it): errors, never an ICE.

use velt_sema::hir::{self, Callee, ExprKind, StmtKind};

use super::c_wasi::{check, lower_checked_for};

const TARGET: &str = "aarch64-apple-darwin";

/// The program's `fcntl` without `...`, called in `main` (`first`) or after `fsyncSync`'s system
/// call of std's variadic `fcntl` is lowered.
fn plain_fcntl(first: bool) -> String {
    let own = "unsafe { fcntl(1, 1) }";
    let (call_main, call_later) = if first { (own, "0") } else { ("0", own) };
    format!(
        "import {{ fsyncSync }} from \"velt/fs\";\n\
         declare c(\"c\") function fcntl(fd: i32, cmd: i32): i32;\n\
         function later(): i32 {{\n  // SAFETY: a test of lowering only; never run.\n  return {call_later};\n}}\n\
         function deeper(): i32 {{\n  return later();\n}}\n\
         function main() {{\n  // SAFETY: a test of lowering only; never run.\n  const a: i32 = {call_main};\n  fsyncSync(1);\n  console.log(a + deeper());\n}}\n"
    )
}

#[test]
fn a_plain_declaration_of_a_variadic_system_call_is_an_error_either_way() {
    for first in [true, false] {
        let src = plain_fcntl(first);
        let (p, sm) = check(&src);
        let e = lower_checked_for(&p, &sm, TARGET).expect_err(&src);
        let want = "C function `fcntl` has two signatures: `fcntl(i32, i32): i32` here, and the variadic signature the compiler's runtime calls it with";
        assert!(e.contains(want), "first: {first}\n{e}");
        assert!(
            e.contains("it is not variadic here and variadic there"),
            "first: {first}\n{e}"
        );
        assert_eq!(e.matches("has two signatures").count(), 1, "{e}");
    }
}

/// A variadic `fcntl` of the program whose fixed parameters differ from std's (`u64` for the
/// command), either way round.
#[test]
fn a_variadic_declaration_with_other_parameters_is_an_error_either_way() {
    for first in [true, false] {
        let src = plain_fcntl(first)
            .replace(
                "fcntl(fd: i32, cmd: i32): i32",
                "fcntl(fd: i32, cmd: u64, ...args: CVarArgs): i32",
            )
            .replace(
                "import {",
                "import { CVarArgs } from \"velt/ffi\";\nimport {",
            );
        let (p, sm) = check(&src);
        let e = lower_checked_for(&p, &sm, TARGET).expect_err(&src);
        let want = "C function `fcntl` has two signatures: `fcntl(i32, u64, ...): i32` here, and the signature the compiler's runtime calls it with";
        assert!(e.contains(want), "first: {first}\n{e}");
        assert!(!e.contains("variadic here"), "{e}");
    }
}

/// The C calls in `b`, wherever they are in the block.
fn c_calls<'a>(b: &'a mut hir::Block, out: &mut Vec<&'a mut Vec<hir::Expr>>) {
    for s in &mut b.stmts {
        match &mut s.kind {
            StmtKind::Expr(e) | StmtKind::Let { init: Some(e), .. } => expr_c_calls(e, out),
            _ => {}
        }
    }
}

fn expr_c_calls<'a>(e: &'a mut hir::Expr, out: &mut Vec<&'a mut Vec<hir::Expr>>) {
    match &mut e.kind {
        ExprKind::Block(b) => c_calls(b, out),
        ExprKind::Call {
            callee: Callee::Def(..),
            args,
        } if args.len() == 2 => out.push(args),
        _ => {}
    }
}

/// Sema leaves a call with too few arguments without any (and reports it); should lowering ever
/// meet one, it reports an error too and indexes no argument that isn't there.
#[test]
fn a_variadic_call_without_its_fixed_arguments_is_an_error() {
    let src = "import { CString, CVarArgs } from \"velt/ffi\";\n\
        declare c(\"c\") function printf(format: CString, ...args: CVarArgs): i32;\n\
        function main() {\n  const f = new CString(\"%d\");\n  \
        // SAFETY: a test of lowering only; never run.\n  \
        const n = unsafe { printf(f, 1 as i32) };\n  console.log(n);\n}\n";
    let (mut p, sm) = check(src);
    let main = p.entry.expect("main");
    let hir::Def::Fn(f) = &mut p.defs[main.0 as usize] else {
        panic!("main is a function")
    };
    let mut calls = vec![];
    c_calls(&mut f.body.block, &mut calls);
    assert_eq!(calls.len(), 1, "the call of printf");
    calls[0].clear();
    let e = lower_checked_for(&p, &sm, TARGET).expect_err(src);
    assert!(
        e.contains(
            "C function `printf` takes at least 1 argument before its `...`, but 0 were supplied"
        ),
        "{e}"
    );
}

/// A package's `fcntl` or `open` of another C signature than std's system calls (velt/fs's
/// `openSync` and `fsyncSync` reach them), its call lowered first: an error at the declaration,
/// each the way it differs.
#[test]
fn declarations_that_differ_from_std_s_system_calls_are_errors() {
    for (decl, call, shown, note) in [
        (
            "fcntl(fd: i32, cmd: i32, arg: i32): i32",
            "fcntl(1, 1, 0)",
            "fcntl(i32, i32, i32): i32` here, and the variadic signature",
            "it is not variadic here and variadic there",
        ),
        (
            "fcntl(fd: i32, cmd: i32, ...args: CVarArgs): i64",
            "fcntl(1, 1) as i32",
            "fcntl(i32, i32, ...): i64` here, and the signature",
            "the compiler's runtime calls this C function itself",
        ),
        (
            "open(path: CString, ...args: CVarArgs): i32",
            "open(new CString(\"x\"))",
            "open(CString, ...): i32` here, and the signature",
            "the compiler's runtime calls this C function itself",
        ),
    ] {
        let src = format!(
            "import {{ CString, CVarArgs }} from \"velt/ffi\";\n\
             import {{ fsyncSync, openSync }} from \"velt/fs\";\n\
             declare c(\"c\") function {decl};\n\
             function main() {{\n  // SAFETY: a test of lowering only; never run.\n  \
             const a: i32 = unsafe {{ {call} }};\n  \
             const fd = openSync(\"x\", \"r\");\n  fsyncSync(fd);\n  console.log(a);\n}}\n"
        );
        let (p, sm) = check(&src);
        let e = lower_checked_for(&p, &sm, TARGET).expect_err(&src);
        assert!(e.contains(shown), "{decl}:\n{e}");
        assert!(e.contains(note), "{decl}:\n{e}");
        assert_eq!(e.matches("has two signatures").count(), 1, "{decl}:\n{e}");
    }
}
