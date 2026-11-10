//! Calls of variadic C functions (`vir::ExternFn::variadic`, function/variadic.rs): every target
//! compiles one with integer and pointer arguments; SysV x86-64 refuses a float one, which needs
//! `%al` set, naming the declaration.

use super::*;
use crate::{emit_object, CodegenOptions};

/// `fn f(p: Ptr) -> i32 { return printf(p, 7, …) }` with `printf` variadic after its format,
/// the further arguments of types `extra`.
fn calls_printf(extra: &[Ty]) -> Program {
    let (mut pb, _) = ProgramBuilder::new();
    let printf = pb.ext("printf", &[Ty::Ptr], Ty::I32, false);
    pb.p.externs[printf.0 as usize].variadic = true;
    pb.p.externs[printf.0 as usize].c_decl = Some("src/lib.velt:3:1".into());
    let mut fb = FuncBuilder::new("f", &[Ty::Ptr], Ty::I32, Linkage::Export);
    let r = fb.local(Ty::I32);
    let b = fb.block();
    let mut args = vec![copy_local(fb.param(0))];
    for &t in extra {
        args.push(match t {
            Ty::F64 => float(2.5, t),
            _ => int(7, t),
        });
    }
    let next = fb.call(b, Callee::Extern(printf), args, Some(Place::local(r)));
    fb.term(next, Terminator::Return(copy_local(r)));
    pb.add(fb.finish());
    let (main, b) = main_fb();
    pb.add(finish_main(main, b, 0));
    pb.p
}

fn emit(program: &Program, target: &str) -> Result<Vec<u8>, String> {
    let opts = CodegenOptions {
        target: target.into(),
        optimize: false,
        cpu: None,
    };
    emit_object(program, &opts)
}

const TARGETS: [&str; 5] = [
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-pc-windows-msvc",
];

#[test]
fn integer_and_pointer_variadic_arguments_compile_on_every_target() {
    // More than eight, so some go on the stack everywhere.
    let extra = [
        Ty::I32,
        Ty::U32,
        Ty::I64,
        Ty::U64,
        Ty::Ptr,
        Ty::I32,
        Ty::I32,
        Ty::I32,
        Ty::I32,
    ];
    let program = calls_printf(&extra);
    velt_vir::verify(&program).unwrap();
    for target in TARGETS {
        emit(&program, target).unwrap_or_else(|e| panic!("{target}: {e}"));
    }
    // None at all, too.
    emit(&calls_printf(&[]), "aarch64-apple-darwin").unwrap();
}

#[test]
fn a_float_variadic_argument_is_refused_only_on_sysv_x86_64() {
    let program = calls_printf(&[Ty::I32, Ty::F64]);
    for target in TARGETS {
        let r = emit(&program, target);
        if target.starts_with("x86_64") && !target.contains("windows") {
            let e = r.expect_err(target);
            assert!(
                e.contains("C function `printf` (declared at src/lib.velt:3:1) is passed a float variadic argument")
                    && e.contains("--release"),
                "{target}: {e}"
            );
        } else {
            r.unwrap_or_else(|e| panic!("{target}: {e}"));
        }
    }
}

/// The refusal stops translating the function half-way; the worker's builder context must be
/// reusable for its next function (it panicked in cranelift-frontend: `func_ctx.is_empty()`).
#[test]
fn a_refused_call_leaves_the_builder_context_reusable() {
    use cranelift_codegen::ir::{self, UserFuncName};
    use cranelift_frontend::FunctionBuilderContext;
    use cranelift_module::default_libcall_names;
    use cranelift_object::{ObjectBuilder, ObjectModule};

    let program = calls_printf(&[Ty::I32, Ty::F64]);
    let isa = crate::isa::make_isa("x86_64-unknown-linux-gnu", None, false, false).unwrap();
    let builder = ObjectBuilder::new(isa.clone(), "velt", default_libcall_names()).unwrap();
    let mut module = ObjectModule::new(builder);
    let (decls, _) =
        crate::module::declare_all(&mut module, &program, &crate::module::Naming::Program).unwrap();
    let target = crate::function::Target::of(&*isa, &module);
    let mut builder_ctx = FunctionBuilderContext::new();
    // `f` (refused) first, then `main` on the same context.
    for (i, f) in program.funcs.iter().enumerate() {
        let mut func = ir::Function::new();
        func.signature = crate::abi::make_signature(decls.call_conv, &f.params, f.ret).unwrap();
        func.name = UserFuncName::user(0, i as u32);
        let r = crate::function::translate_function(
            &target,
            &program,
            &decls,
            f,
            &mut func,
            &mut builder_ctx,
        );
        assert_eq!(r.is_err(), f.symbol == "f", "{}", f.symbol);
    }
}
