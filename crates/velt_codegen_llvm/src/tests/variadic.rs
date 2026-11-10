//! Calls of variadic C functions (`vir::ExternFn::variadic`): the declaration ends in `...` and
//! each call states the function type, so LLVM passes the further arguments as the target's C
//! does (on SysV x86-64 it sets `%al`, which no run of the program can be relied on to show).

use super::*;

/// `velt_main` calling `printf(format, 7 as i32, 2.5)`, `printf` variadic after its format.
fn calls_printf() -> Program {
    let mut pb = ProgramBuilder::new();
    let printf = pb.ext("printf", &[Ptr], I32, false);
    pb.p.externs[printf.0 as usize].variadic = true;
    let format = pb.stat(b"%d %f\n\0", 1);
    let mut fb = FuncBuilder::export("velt_main", &[], I32);
    let b = fb.block();
    let r = fb.local(I32);
    let args = vec![
        Operand::Const(Const::Static(format), Ptr),
        int(7, I32),
        float(2.5, F64),
    ];
    let b = fb.call(b, Callee::Extern(printf), args, Some(r));
    fb.ret(b, int(0, I32));
    pb.add(fb.finish());
    pb.finish()
}

#[test]
fn a_variadic_extern_is_declared_and_called_with_its_dots() {
    for triple in [
        "x86_64-unknown-linux-gnu",
        "aarch64-apple-darwin",
        "x86_64-pc-windows-msvc",
    ] {
        let ir = emit_ir(&calls_printf(), triple).unwrap();
        let declare = ir
            .lines()
            .find(|l| l.starts_with("declare") && l.contains("printf"))
            .unwrap_or_else(|| panic!("{triple}: no declaration of printf in\n{ir}"));
        assert!(declare.contains("(ptr, ...)"), "{triple}: {declare}");
        let call = ir
            .lines()
            .find(|l| l.contains("call") && l.contains("printf"))
            .unwrap_or_else(|| panic!("{triple}: no call of printf in\n{ir}"));
        assert!(call.contains("call i32 (ptr, ...) @"), "{triple}: {call}");
        assert!(
            call.contains("i32 7") && call.contains("double"),
            "{triple}: {call}"
        );
    }
}
