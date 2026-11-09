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

/// The name of static `i` of `p` (`crate::statics::static_names`: from its contents).
fn static_name(p: &velt_vir::vir::Program, i: usize) -> String {
    crate::statics::static_names(p)[i].clone()
}

#[test]
fn module_structure() {
    let ir = ir();
    let boom = format!(
        "{} = private unnamed_addr constant [6 x i8] c\"boom\\22\\0A\", align 1",
        static_name(&sample(), 0)
    );
    for needle in [
        "target triple = \"x86_64-unknown-linux-gnu\"",
        boom.as_str(),
        "declare void @\"velt_rt_panic\"(ptr) #2",
        "define internal zeroext i8 @\"helper\"(i8 signext %p0, i16 zeroext %p1) #0 {",
        "define dso_local i32 @\"velt_main\"() #0 {",
        "\"probe-stack\"=\"inline-asm\"",
        "attributes #2 = { noreturn }",
    ] {
        assert!(ir.contains(needle), "missing `{needle}` in\n{ir}");
    }
}

#[test]
fn functions_that_never_return_are_cold() {
    // `oob` has no `return` (a panic helper); `fail` returns only its error (`Function::cold`,
    // a `never` function that throws); `helper` returns.
    let mut p = sample();
    let panic = ExternId(0);
    let mut fb = FuncBuilder::internal("oob", &[Ptr], Unit);
    let b = fb.block();
    let n = fb.call(
        b,
        Callee::Extern(panic),
        vec![copy_local(fb.param(0))],
        None,
    );
    fb.term(n, Terminator::Unreachable);
    p.funcs.push(fb.finish());
    let mut fb = FuncBuilder::internal("fail", &[Ptr], Unit);
    let b = fb.block();
    fb.ret(b, Operand::Const(Const::Unit, Unit));
    let mut fail = fb.finish();
    fail.cold = true;
    p.funcs.push(fail);
    let ir = emit_ir(&p, "x86_64-unknown-linux-gnu").expect("emit_ir");
    for needle in [
        "define internal void @\"oob\"(ptr %p0) #0 cold noinline {",
        "define internal void @\"fail\"(ptr %p0) #0 cold noinline {",
        "define internal zeroext i8 @\"helper\"(i8 signext %p0, i16 zeroext %p1) #0 {",
    ] {
        assert!(ir.contains(needle), "missing `{needle}` in\n{ir}");
    }
}

#[test]
fn inline_markers_give_their_attributes() {
    // std's markers (`Function::inline`): `@inline` is `alwaysinline`, `@noInline` is
    // `noinline` without `cold`; a function that never returns stays `cold noinline` even when
    // marked `@inline` (the attributes conflict).
    let mut p = sample();
    for (name, marker, cold) in [
        ("always", Inline::Always, false),
        ("never", Inline::Never, false),
        ("both", Inline::Always, true),
    ] {
        let mut fb = FuncBuilder::internal(name, &[Ptr], Unit);
        let b = fb.block();
        fb.ret(b, Operand::Const(Const::Unit, Unit));
        let mut f = fb.finish();
        f.inline = marker;
        f.cold = cold;
        p.funcs.push(f);
    }
    let ir = emit_ir(&p, "x86_64-unknown-linux-gnu").expect("emit_ir");
    for needle in [
        "define internal void @\"always\"(ptr %p0) #0 alwaysinline {",
        "define internal void @\"never\"(ptr %p0) #0 noinline {",
        "define internal void @\"both\"(ptr %p0) #0 cold noinline {",
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

/// `sample()` with source locations: `helper` in `b.velt`, one `velt_main` statement too.
fn located_sample() -> Program {
    let mut p = sample();
    p.files = vec!["a.velt".into(), "C:/src/b.velt".into()];
    let at = |file, line| Some(SrcLoc { file, line, col: 5 });
    for f in &mut p.funcs {
        let file = if f.symbol == "helper" { 1 } else { 0 };
        f.locs = f
            .blocks
            .iter()
            .map(|b| vec![at(file, 7); b.stmts.len() + 1])
            .collect();
    }
    // A statement inlined from `b.velt` into `velt_main`.
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
        "!0 = !DIFile(filename: \"a.velt\", directory: ",
        "!1 = !DIFile(filename: \"b.velt\", directory: \"C:/src\")",
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
    let p = vtable_sample();
    let ir = emit_ir(&p, "x86_64-pc-windows-msvc").unwrap();
    let vtable = format!(
        "{} = private unnamed_addr constant <{{ [8 x i8], ptr, ptr, ptr, [8 x i8] }}> \
         <{{ [8 x i8] c\"header!!\", ptr @\"helper\", ptr @\"velt_rt_panic\", ptr {}, \
         [8 x i8] c\"\\00\\00\\00\\00\\00\\00\\00\\7F\" }}>, align 8",
        static_name(&p, 1),
        static_name(&p, 0)
    );
    for needle in [
        vtable.as_str(),
        "declare void @llvm.memset.p0.i64(ptr, i8, i64, i1)",
        "call void @llvm.memset.p0.i64(ptr align 1 %",
        ", i8 -85, i64 %",
        "declare void @llvm.memmove.p0.p0.i64(ptr, ptr, i64, i1)",
        "call void @llvm.memcpy.p0.p0.i64(ptr align 1 %",
    ] {
        assert!(ir.contains(needle), "missing `{needle}` in\n{ir}");
    }
}

/// `velt_main` using runtime functions the backend knows: math with an exact intrinsic and
/// `Math.round` (an inline helper), as statements; a read-only string compare and the allocator.
fn runtime_sample() -> Program {
    let mut pb = ProgramBuilder::new();
    pb.ext("velt_rt_math_sqrt", &[F64], F64, false);
    pb.ext("velt_rt_math_round", &[F64], F64, false);
    let cmp = pb.ext("velt_rt_str_cmp", &[Ptr, Ptr], I32, false);
    let alloc = pb.ext("velt_rt_alloc", &[U64, U64], Ptr, false);
    let mut fb = FuncBuilder::export("velt_main", &[], I32);
    let (x, y, c, p) = (fb.local(F64), fb.local(F64), fb.local(I32), fb.local(Ptr));
    let b = fb.block();
    let math = |op| Intrinsic::Math(op);
    let sqrt = Rvalue::Intrinsic(math(velt_vir::math::MathOp::Sqrt), vec![float(2.0, F64)]);
    fb.assign(b, x, sqrt);
    let round = Rvalue::Intrinsic(math(velt_vir::math::MathOp::Round), vec![copy_local(x)]);
    fb.assign(b, y, round);
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
    // The baseline x86-64 CPU (`--cpu baseline`) has no rounding instruction: `velt.floor` & co.
    let baseline = crate::target::normalize("x86_64-unknown-linux-gnu")
        .and_then(|t| t.with_cpu(Some("baseline")))
        .unwrap();
    let ir = crate::checked_ir(&runtime_sample(), &baseline, false).unwrap();
    for needle in [
        "declare double @llvm.sqrt.f64(double)",
        "call double @llvm.sqrt.f64(double 0x4000000000000000)",
        "call double @velt.round(double %",
        "define internal double @velt.round(double %x) alwaysinline nounwind {",
        "define internal double @velt.floor(double %x) alwaysinline nounwind {",
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
    assert!(!ir.contains("call double @\"velt_rt_math_round\""));
    // Elsewhere, x86_64 at its default level (x86-64-v2, SSE4.1's `roundsd`) included,
    // `Math.round` floors with the one-instruction intrinsic.
    for target in ["aarch64-unknown-linux-gnu", "x86_64-unknown-linux-gnu"] {
        let ir = emit_ir(&runtime_sample(), target).unwrap();
        assert!(
            ir.contains("call double @llvm.floor.f64(double %x)"),
            "{target}: {ir}"
        );
        assert!(!ir.contains("@velt.floor"), "{target}");
    }
}

#[test]
fn x86_code_is_for_the_default_cpu_level_unless_asked() {
    let t = |cpu: Option<&str>| crate::target::normalize("x86_64-pc-windows-msvc")?.with_cpu(cpu);
    let arg = |cpu| t(cpu).unwrap().cpu_arg();
    assert_eq!(arg(None).as_deref(), Some("-march=x86-64-v2"));
    assert_eq!(arg(Some("x86-64-v3")).as_deref(), Some("-march=x86-64-v3"));
    assert_eq!(arg(Some("baseline")).as_deref(), Some("-march=x86-64"));
    // An LLVM CPU name goes to clang as it is.
    assert_eq!(arg(Some("znver4")).as_deref(), Some("-march=znver4"));
    assert!(t(Some("Znver4")).is_err());
    assert!(!t(None).unwrap().rounds_by_conversion());
    assert!(t(Some("baseline")).unwrap().rounds_by_conversion());
    let arm = |cpu| crate::target::normalize("aarch64-unknown-linux-gnu")?.with_cpu(cpu);
    assert_eq!(arm(None).unwrap().cpu_arg(), None);
    assert_eq!(arm(Some("baseline")).unwrap().cpu_arg(), None);
    assert_eq!(
        arm(Some("neoverse-n1")).unwrap().cpu_arg().as_deref(),
        Some("-mcpu=neoverse-n1")
    );
    assert!(arm(Some("x86-64-v2")).is_err());
    assert!(
        arm(Some("znver5")).is_err(),
        "never reaches clang as -mcpu=znver5"
    );
    assert!(t(Some("skylkae")).is_err() && t(Some("neoverse-n1")).is_err());
    if crate::available() {
        // What clang enables for a CPU name, not Cranelift's preset of it (which has no AVX2).
        const AVX2: u32 = 1 << 7;
        for cpu in ["alderlake", "znver4"] {
            let target = t(Some(cpu)).unwrap();
            assert_ne!(target.cpu_features & AVX2, 0, "{cpu}");
            assert!(!target.rounds_by_conversion(), "{cpu}");
            assert_eq!(
                crate::required_features(&target.triple, Some(cpu)),
                Ok(target.cpu_features)
            );
        }
    }
    let host = crate::target::normalize("")
        .unwrap()
        .with_cpu(Some("native"));
    let native = host.unwrap().cpu_arg().unwrap();
    assert!(
        native == "-march=native" || native == "-mcpu=native",
        "{native}"
    );
    // WebAssembly has no CPU choice.
    let wasm = crate::target::normalize("wasm32-wasip1").unwrap();
    assert_eq!(wasm.with_cpu(Some("x86-64-v3")).unwrap().cpu_arg(), None);
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
            frozen: false,
            nonnull: true,
            dereferenceable: 24,
        },
        ParamAttrs {
            noalias: false,
            readonly: true,
            frozen: false,
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

mod aes;
mod atomics;
mod half;
mod inlining;
mod placement;
mod static_symbols;
mod target_features;
mod tbaa;
mod units;
mod wasm;

/// `velt_main` returning `velt_simd_has_sha_x86()` (#1981 step 5).
fn sha_check_sample() -> Program {
    let mut pb = ProgramBuilder::new();
    let has = pb.ext("velt_simd_has_sha_x86", &[], Bool, false);
    let mut fb = FuncBuilder::export("velt_main", &[], I32);
    let (r, w) = (fb.local(Bool), fb.local(I32));
    let b = fb.block();
    let b2 = fb.call(b, Callee::Extern(has), vec![], Some(r));
    fb.assign(b2, w, Rvalue::Cast(copy_local(r), I32));
    fb.ret(b2, copy_local(w));
    pb.add(fb.finish());
    pb.finish()
}

#[test]
fn extension_checks_are_true_on_a_cpu_built_for_with_the_extension() {
    let p = sha_check_sample();
    let target = crate::target::normalize("x86_64-unknown-linux-gnu").unwrap();
    // The default CPU (x86-64-v2) may lack SHA: the check reads the runtime's flag.
    let checked = crate::checked_ir(&p, &target, false).unwrap();
    assert!(checked.contains("@velt_cpu_sha_x86"), "{checked}");
    // Built for a CPU with SHA (`--cpu`), whose start-up check checks it: the check is true.
    let mut sha = target.clone();
    sha.cpu_features |= velt_codegen_cl::ext_x86_features(velt_vir::simd::Ext::ShaX86);
    let folded = crate::checked_ir(&p, &sha, false).unwrap();
    assert!(!folded.contains("@velt_cpu_sha_x86"), "{folded}");
    // SHA alone, without the SSSE3 and SSE4.1 the extension also needs, is not enough.
    let mut partial = target.clone();
    partial.cpu_features = 1 << 21;
    let partial = crate::checked_ir(&p, &partial, false).unwrap();
    assert!(partial.contains("@velt_cpu_sha_x86"), "{partial}");
}

/// `velt_main` returning lane 0 of `i32x4.dotAdd` on zeros.
fn dot_sample() -> Program {
    let mut pb = ProgramBuilder::new();
    let dot = pb.ext("velt_simd_i32x4_dot_add", &[V128, V128, V128], V128, false);
    let lane = pb.ext("velt_simd_i32x4_lane", &[V128, U64], I32, false);
    let mut fb = FuncBuilder::export("velt_main", &[], I32);
    let (v, r) = (fb.local(V128), fb.local(I32));
    let b = fb.block();
    let zero = Operand::Const(Const::Int(0), V128);
    let b = fb.call(
        b,
        Callee::Extern(dot),
        vec![zero.clone(), zero.clone(), zero],
        Some(v),
    );
    let b = fb.call(
        b,
        Callee::Extern(lane),
        vec![copy_local(v), int(0, U64)],
        Some(r),
    );
    fb.ret(b, copy_local(r));
    pb.add(fb.finish());
    pb.finish()
}

#[test]
fn the_dot_product_is_sdot_where_every_cpu_has_it() {
    let p = dot_sample();
    let ir = |triple: &str| emit_ir(&p, triple).expect("emit_ir");
    // Every Apple arm64 CPU has Arm's dot-product instructions.
    assert!(ir("aarch64-apple-darwin").contains("@llvm.aarch64.neon.sdot.v4i32.v16i8"));
    // Other arm64 CPUs may lack them, and x86 has no such instruction: widening code.
    for triple in ["aarch64-unknown-linux-gnu", "x86_64-unknown-linux-gnu"] {
        let text = ir(triple);
        assert!(!text.contains("sdot"), "{triple}");
        assert!(text.contains("sext <16 x i8>"), "{triple}");
    }
}

/// `velt_main` returning 0 after calling the SIMD extern `symbol`, of `count` vectors.
fn vector_call_sample(symbol: &str, count: usize) -> Program {
    let mut pb = ProgramBuilder::new();
    let callee = pb.ext(symbol, &vec![V128; count], V128, false);
    let mut fb = FuncBuilder::export("velt_main", &[], I32);
    let v = fb.local(V128);
    let b = fb.block();
    let zero = Operand::Const(Const::Int(0), V128);
    let b = fb.call(b, Callee::Extern(callee), vec![zero; count], Some(v));
    fb.ret(b, int(0, I32));
    pb.add(fb.finish());
    pb.finish()
}

/// `fmlal` on Apple arm64 and where `--cpu` has FEAT_FHM, `bfdot` and `vdpbf16ps` only where
/// `--cpu` has them; the CPU's word comes from clang.
#[test]
fn half_dot_products_follow_the_cpu() {
    if !crate::available() {
        return;
    }
    let ir = |symbol: &str, triple: &str, cpu: Option<&str>| {
        let t = crate::target::normalize(triple)
            .unwrap()
            .with_cpu(cpu)
            .unwrap();
        crate::checked_ir(&vector_call_sample(symbol, 3), &t, false).unwrap()
    };
    let f16 = "velt_simd_f32x4_dot_add_f16x8";
    let fast = "velt_simd_f32x4_dot_add_fast_bf16x8";
    let (apple, linux) = ("aarch64-apple-darwin", "aarch64-unknown-linux-gnu");
    let x86 = "x86_64-unknown-linux-gnu";
    // Every Apple arm64 CPU has FEAT_FHM, not every one FEAT_BF16 (the M1 has not).
    for cpu in [None, Some("baseline"), Some("apple-m1")] {
        assert!(ir(f16, apple, cpu).contains("neon.fmlal2"), "{cpu:?}");
        assert!(!ir(fast, apple, cpu).contains("bfdot"), "{cpu:?}");
    }
    assert!(ir(fast, apple, Some("apple-m2")).contains("neon.bfdot"));
    // An ARMv8.0 CPU has neither, on Apple's target too; Linux with no `--cpu` neither.
    for (triple, cpu) in [
        (apple, Some("cortex-a53")),
        (linux, None),
        (linux, Some("baseline")),
    ] {
        let text = ir(f16, triple, cpu);
        assert!(
            !text.contains("fmlal") && text.contains("@llvm.fma"),
            "{triple} {cpu:?}"
        );
        assert!(!ir(fast, triple, cpu).contains("bfdot"), "{triple} {cpu:?}");
    }
    assert!(ir(f16, linux, Some("neoverse-v1")).contains("neon.fmlal2"));
    assert!(ir(fast, linux, Some("neoverse-v1")).contains("neon.bfdot"));
    // x86: only a CPU with AVX512-BF16; no level has it.
    for cpu in [
        None,
        Some("x86-64-v3"),
        Some("x86-64-v4"),
        Some("skylake-avx512"),
    ] {
        assert!(!ir(fast, x86, cpu).contains("dpbf16ps"), "{cpu:?}");
    }
    assert!(ir(fast, x86, Some("sapphirerapids")).contains("avx512bf16.dpbf16ps.128"));
    assert!(ir(fast, x86, Some("cooperlake")).contains("avx512bf16.dpbf16ps.128"));
}

/// `<8 x half>` where the CPU has the lane arithmetic, by `--cpu` as for the dot products, and
/// `<4 x float>` elsewhere.
#[test]
fn f16_arithmetic_follows_the_cpu() {
    if !crate::available() {
        return;
    }
    let ir = |triple: &str, cpu: Option<&str>| {
        let t = crate::target::normalize(triple)
            .unwrap()
            .with_cpu(cpu)
            .unwrap();
        let sample = vector_call_sample("velt_simd_f16x8_add", 2);
        crate::checked_ir(&sample, &t, false).unwrap()
    };
    let (apple, linux) = ("aarch64-apple-darwin", "aarch64-unknown-linux-gnu");
    let x86 = "x86_64-unknown-linux-gnu";
    for (triple, cpu) in [
        (apple, None),
        (apple, Some("baseline")),
        (linux, Some("neoverse-v1")),
        (x86, Some("sapphirerapids")),
    ] {
        assert!(
            ir(triple, cpu).contains("fadd <8 x half>"),
            "{triple} {cpu:?}"
        );
    }
    for (triple, cpu) in [
        (apple, Some("cortex-a53")),
        (linux, None),
        (linux, Some("baseline")),
        (x86, None),
        (x86, Some("x86-64-v3")),
        (x86, Some("x86-64-v4")),
        (x86, Some("skylake-avx512")),
        ("wasm32-wasip1", None),
    ] {
        let text = ir(triple, cpu);
        assert!(
            text.contains("fadd <4 x float>") && !text.contains("<8 x half>"),
            "{triple} {cpu:?}"
        );
    }
}

#[test]
fn an_arm64_cpu_with_the_dot_product_gets_sdot_anywhere() {
    if !crate::available() {
        return;
    }
    // What clang says each CPU has (ARM_FEATURES' bit 0 is DOTPROD).
    let linux = "aarch64-unknown-linux-gnu";
    assert_eq!(crate::required_features(linux, Some("neoverse-n1")), Ok(1));
    assert_eq!(crate::required_features(linux, Some("cortex-a53")), Ok(0));
    assert_eq!(crate::required_features(linux, Some("baseline")), Ok(0));
    let target = |cpu| {
        crate::target::normalize(linux)
            .unwrap()
            .with_cpu(Some(cpu))
            .unwrap()
    };
    assert!(target("neoverse-n1").has_dotprod());
    assert!(!target("cortex-a53").has_dotprod());
    // A `--cpu` names the CPU on Apple's target too: one without the extension gets none.
    let apple = |cpu| {
        let t = crate::target::normalize("aarch64-apple-darwin").unwrap();
        t.with_cpu(cpu).unwrap()
    };
    assert!(apple(None).has_dotprod() && apple(Some("baseline")).has_dotprod());
    assert!(apple(Some("apple-m4")).has_dotprod());
    assert!(!apple(Some("cortex-a53")).has_dotprod());
    // And the emitted code follows: `sdot` for a dotprod CPU on Linux, none for an ARMv8.0 one
    // on Apple's target.
    let p = dot_sample();
    let ir = |t: &crate::target::Target| crate::checked_ir(&p, t, false).unwrap();
    assert!(ir(&target("neoverse-n1")).contains("@llvm.aarch64.neon.sdot"));
    assert!(!ir(&apple(Some("cortex-a53"))).contains("sdot"));
}
