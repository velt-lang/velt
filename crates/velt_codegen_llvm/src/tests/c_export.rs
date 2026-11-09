//! The functions C calls (`vir::Function::stops_unwind`, `@export("c_name")`): their calls are
//! `invoke`s whose landing pad aborts, with the personality of the target's panics, and their
//! small integer parameters carry no extension attribute.

use super::*;

/// `velt_main`, an internal `body(i8, u16, bool) -> i16` and the export `ex_widen` calling it.
fn sample() -> Program {
    let mut pb = ProgramBuilder::new();
    let body = {
        let mut fb = FuncBuilder::internal("body", &[I8, U16, Bool], I16);
        let b = fb.block();
        let r = fb.local(I16);
        fb.assign(b, r, Rvalue::Cast(copy_local(fb.param(0)), I16));
        fb.ret(b, copy_local(r));
        pb.add(fb.finish())
    };
    let mut fb = FuncBuilder::export("ex_widen", &[I8, U16, Bool], I16);
    let b = fb.block();
    let r = fb.local(I16);
    let args = (0..3).map(|i| copy_local(fb.param(i))).collect();
    let b = fb.call(b, Callee::Func(body), args, Some(r));
    fb.ret(b, copy_local(r));
    let mut export = fb.finish();
    export.stops_unwind = true;
    pb.add(export);
    let mut fb = FuncBuilder::export("velt_main", &[], I32);
    let b = fb.block();
    fb.ret(b, int(0, I32));
    pb.add(fb.finish());
    pb.finish()
}

fn define<'a>(ir: &'a str, name: &str) -> &'a str {
    let at = ir
        .find(&format!("@\"{name}\"("))
        .or_else(|| ir.find(&format!("@{name}(")))
        .unwrap_or_else(|| panic!("no `{name}` in\n{ir}"));
    let start = ir[..at].rfind("\ndefine").map_or(0, |i| i + 1);
    let end = ir[start..]
        .find("\n}\n")
        .map_or(ir.len(), |i| start + i + 3);
    &ir[start..end]
}

#[test]
fn an_export_s_calls_abort_when_a_panic_unwinds_into_them() {
    for (triple, personality, pad) in [
        (
            "x86_64-unknown-linux-gnu",
            "@rust_eh_personality",
            "landingpad { ptr, i32 } cleanup",
        ),
        (
            "aarch64-unknown-linux-gnu",
            "@rust_eh_personality",
            "landingpad { ptr, i32 } cleanup",
        ),
        (
            "aarch64-apple-darwin",
            "@rust_eh_personality",
            "landingpad { ptr, i32 } cleanup",
        ),
        (
            "x86_64-pc-windows-msvc",
            "@__CxxFrameHandler3",
            "cleanuppad within none []",
        ),
    ] {
        let ir = emit_ir(&sample(), triple).unwrap();
        let f = define(&ir, "ex_widen");
        let first = f.lines().next().unwrap();
        assert!(
            first.contains(&format!("personality ptr {personality}")),
            "{triple}:\n{f}"
        );
        // An `invoke`, never a call LLVM could turn into a jump to `body`.
        assert!(f.contains("invoke signext i16 @"), "{triple}:\n{f}");
        assert!(f.contains("unwind label %velt.abort"), "{triple}:\n{f}");
        assert!(!f.contains(" call signext i16 @"), "{triple}:\n{f}");
        assert!(
            f.contains(pad) && f.contains("call void @abort()"),
            "{triple}:\n{f}"
        );
        assert!(
            ir.contains(&format!("declare i32 {personality}(...)")),
            "{triple}"
        );
        assert!(
            ir.contains("declare void @abort() noreturn nounwind"),
            "{triple}"
        );
        // The landing pad needs unwind information.
        assert!(ir.contains("attributes #10 = { uwtable"), "{triple}:\n{ir}");
        // Other functions are as before.
        let main = define(&ir, "velt_main");
        assert!(!main.contains("personality"), "{triple}:\n{main}");
    }
}

/// AAPCS64 outside Darwin leaves the upper bits of a small integer argument unspecified, and
/// clang's callers there don't extend it: an export extends its parameters itself. Its result
/// keeps its attribute (the caller reads it as C's type).
#[test]
fn an_export_s_small_integer_parameters_have_no_extension_attribute() {
    for triple in [
        "aarch64-unknown-linux-gnu",
        "x86_64-unknown-linux-gnu",
        "aarch64-apple-darwin",
    ] {
        let ir = emit_ir(&sample(), triple).unwrap();
        let first = define(&ir, "ex_widen").lines().next().unwrap();
        assert!(
            first.contains("define dso_local signext i16 @")
                && first.contains("(i8 %p0, i16 %p1, i8 %p2)"),
            "{triple}: {first}"
        );
        // Velt's own functions keep them: Velt callers extend.
        let body = define(&ir, "body").lines().next().unwrap();
        assert!(
            body.contains("(i8 signext %p0, i16 zeroext %p1, i8 zeroext %p2)"),
            "{triple}: {body}"
        );
    }
}
