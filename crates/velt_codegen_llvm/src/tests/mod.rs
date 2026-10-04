//! Unit tests on the emitted IR text (no clang needed). Execution against the reference
//! interpreter and per-target object emission are in `tests/native`.

// Shared with velt_opt's tests; only part of the builder is used here.
#[allow(dead_code)]
#[path = "../../../velt_opt/tests/common/builder.rs"]
mod builder;

use builder::*;
use velt_vir::vir::Ty::*;
use velt_vir::vir::*;

use crate::emit_ir;

/// `velt_main` exercising casts, shifts, division, float remainder, a switch, an aggregate
/// copy and a noreturn extern call.
fn sample() -> Program {
    let mut pb = ProgramBuilder::new();
    let panic = pb.ext("velt_rt_panic", &[Ptr], Unit, true);
    let msg = pb.stat(b"boom\"\n", 1);
    let helper = {
        let mut fb = FuncBuilder::internal("helper", &[I8, U16], Bool);
        let b = fb.block();
        let c = fb.local(Bool);
        fb.assign(b, c, bin(BinOp::Lt, int(1, I64), int(2, I64)));
        fb.ret(b, copy_local(c));
        pb.add(fb.finish())
    };
    let mut fb = FuncBuilder::export("velt_main", &[], I32);
    let b = fb.block();
    let (x, s, q, r, f, k) = (
        fb.local(I64),
        fb.local(I32),
        fb.local(I32),
        fb.local(F64),
        fb.local(U8),
        fb.local(Bool),
    );
    let (agg, agg2) = (fb.local(Agg(STR_AGG)), fb.local(Agg(STR_AGG)));
    fb.assign(b, x, Rvalue::Cast(float(1e30, F64), I64));
    fb.assign(b, f, Rvalue::Cast(float(-3.5, F32), U8));
    fb.assign(b, s, bin(BinOp::Shl, int(1, I32), copy_local(x)));
    fb.assign(b, q, bin(BinOp::Div, copy_local(s), int(-1, I32)));
    fb.assign(b, r, bin(BinOp::Rem, float(5.5, F64), float(2.0, F64)));
    let fields = vec![int(0, U64), int(1, U64), int(2, U64)];
    fb.assign(b, agg, Rvalue::Aggregate(STR_AGG, fields));
    fb.assign(b, agg2, Rvalue::Use(copy_local(agg)));
    let args = vec![int(-1, I8), int(65535, U16)];
    let b = fb.call(b, Callee::Func(helper), args, Some(k));
    let (ok, bad) = (fb.block(), fb.block());
    fb.term(
        b,
        Terminator::Switch {
            value: copy_local(q),
            cases: vec![(-1, bad), (7, bad)],
            default: ok,
        },
    );
    fb.ret(ok, int(0, I32));
    let msg_ptr = Operand::Const(Const::Static(msg), Ptr);
    fb.call(bad, Callee::Extern(panic), vec![msg_ptr], None);
    pb.add(fb.finish());
    pb.finish()
}

fn ir() -> String {
    emit_ir(&sample(), "x86_64-unknown-linux-gnu").expect("emit_ir")
}

#[test]
fn module_structure() {
    let ir = ir();
    for needle in [
        "target triple = \"x86_64-unknown-linux-gnu\"",
        "@.s0 = private unnamed_addr constant [6 x i8] c\"boom\\22\\0A\", align 1",
        "declare void @\"velt_rt_panic\"(ptr) #2",
        "define internal zeroext i8 @\"helper\"(i8 signext %p0, i16 zeroext %p1) #0 {",
        "define dso_local i32 @\"velt_main\"() #0 {",
        "\"probe-stack\"=\"inline-asm\"",
        "attributes #2 = { noreturn nounwind }",
    ] {
        assert!(ir.contains(needle), "missing `{needle}` in\n{ir}");
    }
}

#[test]
fn operations_follow_vir_semantics() {
    let ir = ir();
    for needle in [
        "call i64 @llvm.fptosi.sat.i64.f64(double 0x46293E5939A08CEA)",
        "call i8 @llvm.fptoui.sat.i8.f32(float 0xC00C000000000000)",
        "declare i64 @llvm.fptosi.sat.i64.f64(double)",
        // Shift amount: truncated to the width, then masked.
        "trunc i64",
        "and i32 %t",
        "icmp eq i32 -1, -1",
        // `%` on f64: the whole-number fast path, `frem` (C `fmod`) for the rest.
        "call double @\"velt.frem.f64\"(double 0x4016000000000000, double 0x4000000000000000)",
        "define internal double @\"velt.frem.f64\"(double %x, double %y) noinline",
        "%ri = srem i64 %xi, %yi",
        "%s = frem double %x, %y",
        "call double @\"velt.frem.f64.slow\"(double %x, double %y)",
        "declare double @llvm.copysign.f64(double, double)",
        "call void @llvm.memcpy.p0.p0.i64(ptr align 8 %l",
        "call zeroext i8 @\"helper\"(i8 signext -1, i16 zeroext -1)",
        "i32 -1, label %bb",
        "unreachable",
    ] {
        assert!(ir.contains(needle), "missing `{needle}` in\n{ir}");
    }
    assert!(
        !ir.contains("nsw") && !ir.contains("nuw"),
        "integer ops must wrap"
    );
}

/// `sample()` with source locations: `helper` in `b.vlt`, one `velt_main` statement too.
fn located_sample() -> Program {
    let mut p = sample();
    p.files = vec!["a.vlt".into(), "C:/src/b.vlt".into()];
    let at = |file, line| Some(SrcLoc { file, line, col: 5 });
    for f in &mut p.funcs {
        let file = if f.symbol == "helper" { 1 } else { 0 };
        f.locs = f
            .blocks
            .iter()
            .map(|b| vec![at(file, 7); b.stmts.len() + 1])
            .collect();
    }
    // A statement inlined from `b.vlt` into `velt_main`.
    p.funcs[1].locs[0][0] = at(1, 3);
    p
}

#[test]
fn debug_metadata_follows_source_locations() {
    assert!(!ir().contains("!dbg"), "no locations, no debug info");
    let ir = emit_ir(&located_sample(), "x86_64-unknown-linux-gnu").unwrap();
    for needle in [
        "!llvm.dbg.cu = !{!2}",
        "!{i32 2, !\"Debug Info Version\", i32 3}",
        "!{i32 7, !\"Dwarf Version\", i32 4}",
        "!0 = !DIFile(filename: \"a.vlt\", directory: ",
        "!1 = !DIFile(filename: \"b.vlt\", directory: \"C:/src\")",
        "distinct !DISubprogram(name: \"helper\", linkageName: \"helper\", scope: !1, file: !1, \
         line: 7,",
        "spFlags: DISPFlagDefinition | DISPFlagLocalToUnit",
        "define dso_local i32 @\"velt_main\"() #0 !dbg !",
        "!DILocation(line: 7, column: 5, scope: !",
        "!DILexicalBlockFile(scope: !",
        ", !dbg !",
    ] {
        assert!(ir.contains(needle), "missing `{needle}` in\n{ir}");
    }
    // Every instruction line inside a function compiled from VIR carries a location (the
    // backend's own helpers, such as `velt.frem.f64`, have no source and no debug info).
    let mut in_helper = false;
    let body_lines = ir.lines().filter(|l| {
        if l.starts_with("define ") {
            in_helper = l.contains("@\"velt.");
        }
        !in_helper && l.starts_with("  ") && !l.trim_end().ends_with(':')
    });
    for line in body_lines {
        assert!(line.contains("!dbg"), "no location on `{line}`");
    }
    let windows = emit_ir(&located_sample(), "x86_64-pc-windows-msvc").unwrap();
    assert!(windows.contains("!{i32 2, !\"CodeView\", i32 1}"));
}

#[test]
fn darwin_keeps_frame_pointers_and_windows_uses_chkstk() {
    let darwin = emit_ir(&sample(), "aarch64-apple-darwin").unwrap();
    assert!(darwin.contains("\"frame-pointer\"=\"non-leaf\""));
    let windows = emit_ir(&sample(), "x86_64-pc-windows-msvc").unwrap();
    assert!(windows.contains("uwtable") && !windows.contains("probe-stack"));
}

#[test]
fn rejects_invalid_input() {
    let mut p = sample();
    p.funcs[0].blocks[0].term = Terminator::Goto(BlockId(99));
    assert!(emit_ir(&p, "").unwrap_err().contains("invalid VIR"));
    assert!(emit_ir(&sample(), "riscv64-unknown-linux-gnu")
        .unwrap_err()
        .contains("unsupported target"));
}

/// `velt_main` with a vtable static {bytes, helper, bytes, static#0, extern} and the dynamic
/// memory statements on a stack buffer.
fn vtable_sample() -> Program {
    let mut pb = ProgramBuilder::new();
    let panic = pb.ext("velt_rt_panic", &[Ptr], Unit, true);
    let msg = pb.stat(b"hi", 1);
    let mut fb = FuncBuilder::internal("helper", &[], Unit);
    let b = fb.block();
    fb.ret(b, Operand::Const(Const::Unit, Unit));
    let helper = pb.add(fb.finish());
    let mut bytes = vec![0u8; 40];
    bytes[..8].copy_from_slice(b"header!!");
    bytes[39] = 0x7F;
    let relocs = vec![
        (24, Const::Static(msg)),
        (8, Const::Func(helper)),
        (16, Const::Extern(panic)),
    ];
    pb.stat_with(&bytes, 8, relocs);
    let mut fb = FuncBuilder::export("velt_main", &[], I32);
    let (buf, p, n) = (fb.local(Agg(STR_AGG)), fb.local(Ptr), fb.local(U64));
    let b = fb.block();
    fb.assign(b, p, Rvalue::AddrOf(Place::local(buf)));
    fb.assign(b, n, Rvalue::Use(int(24, U64)));
    let (dst, len) = (copy_local(p), copy_local(n));
    let byte = int(0xAB, U8);
    fb.push(b, Stmt::MemSet { dst, byte, len });
    for overlapping in [false, true] {
        let (dst, src, len) = (copy_local(p), copy_local(p), int(0, U64));
        let copy = Stmt::MemCopyDyn {
            dst,
            src,
            len,
            overlapping,
        };
        fb.push(b, copy);
    }
    fb.ret(b, int(0, I32));
    pb.add(fb.finish());
    pb.finish()
}

#[test]
fn relocated_statics_and_memory_intrinsics() {
    let ir = emit_ir(&vtable_sample(), "x86_64-pc-windows-msvc").unwrap();
    for needle in [
        "@.s1 = private unnamed_addr constant <{ [8 x i8], ptr, ptr, ptr, [8 x i8] }> \
         <{ [8 x i8] c\"header!!\", ptr @\"helper\", ptr @\"velt_rt_panic\", ptr @.s0, \
         [8 x i8] c\"\\00\\00\\00\\00\\00\\00\\00\\7F\" }>, align 8",
        "declare void @llvm.memset.p0.i64(ptr, i8, i64, i1)",
        "call void @llvm.memset.p0.i64(ptr align 1 %",
        ", i8 -85, i64 %",
        "declare void @llvm.memmove.p0.p0.i64(ptr, ptr, i64, i1)",
        "call void @llvm.memcpy.p0.p0.i64(ptr align 1 %",
    ] {
        assert!(ir.contains(needle), "missing `{needle}` in\n{ir}");
    }
}

/// `velt_main` calling runtime functions the backend knows: math with an exact intrinsic,
/// `Math.round` (no exact intrinsic, but pure), a read-only string compare and the allocator.
fn runtime_sample() -> Program {
    let mut pb = ProgramBuilder::new();
    let sqrt = pb.ext("velt_rt_math_sqrt", &[F64], F64, false);
    let round = pb.ext("velt_rt_math_round", &[F64], F64, false);
    let cmp = pb.ext("velt_rt_str_cmp", &[Ptr, Ptr], I32, false);
    let alloc = pb.ext("velt_rt_alloc", &[U64, U64], Ptr, false);
    let mut fb = FuncBuilder::export("velt_main", &[], I32);
    let (x, y, c, p) = (fb.local(F64), fb.local(F64), fb.local(I32), fb.local(Ptr));
    let b = fb.block();
    let b = fb.call(b, Callee::Extern(sqrt), vec![float(2.0, F64)], Some(x));
    let b = fb.call(b, Callee::Extern(round), vec![copy_local(x)], Some(y));
    let size = vec![int(8, U64), int(8, U64)];
    let b = fb.call(b, Callee::Extern(alloc), size, Some(p));
    let b = fb.call(
        b,
        Callee::Extern(cmp),
        vec![copy_local(p), copy_local(p)],
        Some(c),
    );
    fb.ret(b, copy_local(c));
    pb.add(fb.finish());
    pb.finish()
}

#[test]
fn runtime_functions_get_intrinsics_and_attributes() {
    let ir = emit_ir(&runtime_sample(), "x86_64-unknown-linux-gnu").unwrap();
    for needle in [
        "declare double @llvm.sqrt.f64(double)",
        "call double @llvm.sqrt.f64(double 0x4000000000000000)",
        "call double @\"velt_rt_math_round\"(double %",
        "declare double @\"velt_rt_math_round\"(double) #3",
        "declare i32 @\"velt_rt_str_cmp\"(ptr, ptr) #4",
        "declare noalias noundef ptr @\"velt_rt_alloc\"(i64 noundef, i64 noundef allocalign) #5",
        "attributes #5 = { nounwind allockind(\"alloc,uninitialized,aligned\") allocsize(0) \
         memory(inaccessiblemem: readwrite) \"alloc-family\"=\"velt_rt_alloc\" }",
        "attributes #6 = { nounwind allockind(\"realloc,aligned\") allocsize(3) \
         memory(argmem: readwrite, inaccessiblemem: readwrite) \"alloc-family\"=\"velt_rt_alloc\" }",
        "attributes #3 = { nounwind willreturn memory(none) }",
        "attributes #4 = { nounwind willreturn memory(read) }",
    ] {
        assert!(ir.contains(needle), "missing `{needle}` in\n{ir}");
    }
    assert!(!ir.contains("call double @\"velt_rt_math_sqrt\""));
}

#[test]
fn param_attributes_become_llvm_attributes() {
    let mut pb = ProgramBuilder::new();
    let mut fb = FuncBuilder::internal("f", &[Ptr, Ptr, I8], Unit);
    let b = fb.block();
    fb.ret(b, Operand::Const(Const::Unit, Unit));
    let mut f = fb.finish();
    f.param_attrs = vec![
        ParamAttrs {
            noalias: true,
            readonly: false,
            nonnull: true,
            dereferenceable: 24,
        },
        ParamAttrs {
            noalias: false,
            readonly: true,
            nonnull: true,
            dereferenceable: 8,
        },
        ParamAttrs::default(),
    ];
    pb.add(f);
    let mut main = FuncBuilder::export("velt_main", &[], I32);
    let b = main.block();
    main.ret(b, int(0, I32));
    pb.add(main.finish());
    let ir = emit_ir(&pb.finish(), "x86_64-unknown-linux-gnu").expect("emit_ir");
    let needle = "(ptr noalias nonnull dereferenceable(24) %p0, \
                  ptr readonly nonnull dereferenceable(8) %p1, i8 signext %p2)";
    assert!(ir.contains(needle), "missing `{needle}` in\n{ir}");
}

mod placement;
mod units;
mod wasm;
