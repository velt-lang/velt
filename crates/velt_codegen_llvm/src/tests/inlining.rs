//! Caps on LLVM's inlining (`inlining`): the size a function can reach in LLVM is bounded, and
//! the calls and functions past the caps are `noinline` in the IR.

use super::builder::*;
use velt_vir::vir::Ty::*;
use velt_vir::vir::*;

use crate::inlining::{Caps, CALLER_LIMIT, SMALL};

/// An internal function of `blocks` blocks, each calling `sink` (cost `blocks`).
fn calling(pb: &mut ProgramBuilder, name: &str, sink: ExternId, blocks: usize) -> FuncId {
    let mut fb = FuncBuilder::internal(name, &[], Unit);
    let mut b = fb.block();
    for k in 1..blocks {
        b = fb.call(b, Callee::Extern(sink), vec![int(k as i128, I64)], None);
    }
    fb.ret(b, Operand::Const(Const::Unit, Unit));
    pb.add(fb.finish())
}

/// `velt_main` calling each of `callees` once, in order.
fn main_calling(pb: &mut ProgramBuilder, callees: &[FuncId]) -> FuncId {
    let mut fb = FuncBuilder::export("velt_main", &[], I32);
    let mut b = fb.block();
    for &f in callees {
        b = fb.call(b, Callee::Func(f), vec![], None);
    }
    fb.ret(b, int(0, I32));
    pb.add(fb.finish())
}

/// A `main` calling 400 functions of 1,500 statements once each (a generated program's entry):
/// LLVM inlined every one into it. Now it takes in what fits under the caller limit, the
/// other calls stay calls.
#[test]
fn a_caller_takes_in_callees_up_to_the_limit() {
    let mut pb = ProgramBuilder::new();
    let sink = pb.ext("sink", &[I64], Unit, false);
    let runs: Vec<FuncId> = (0..400)
        .map(|i| calling(&mut pb, &format!("run{i}"), sink, 1_500))
        .collect();
    let main = main_calling(&mut pb, &runs);
    let p = pb.finish();
    let caps = Caps::of(&p);
    let main = main.0 as usize;
    let own = crate::inlining::cost(&p.funcs[main]);
    let inlined = ((CALLER_LIMIT - own) / 1_500) as usize;
    assert_eq!(caps.kept(main).len(), 400 - inlined);
    // The calls kept are the last ones (blocks are in call order).
    assert_eq!(caps.kept(main)[0], inlined);
    assert!(caps.grown[main] <= CALLER_LIMIT, "{}", caps.grown[main]);
    let ir = crate::emit_ir(&p, "x86_64-unknown-linux-gnu").unwrap();
    let kept = ir.matches("() noinline").count();
    assert_eq!(kept, 400 - inlined, "{}", &ir[..2_000]);
    assert!(ir.contains(&format!("call void @\"run{}\"() noinline", inlined)));
    assert!(ir.contains("call void @\"run0\"()\n"));
    // Each run is small enough to inline: no `noinline` on its definition.
    assert!(!ir.contains("@\"run0\"() #0 noinline"));
}

/// A function that can grow past the caller limit, by itself or through the small callees it
/// takes in, is `noinline`; one below it is not.
#[test]
fn large_functions_are_noinline() {
    let mut pb = ProgramBuilder::new();
    let sink = pb.ext("sink", &[I64], Unit, false);
    let big = calling(&mut pb, "big", sink, CALLER_LIMIT as usize + 1);
    let fits = calling(&mut pb, "fits", sink, CALLER_LIMIT as usize - 100);
    // 9,990 of its own, plus 20 callees of 40: 10,790 when LLVM has inlined them.
    let small = calling(&mut pb, "small", sink, SMALL as usize);
    let grows = {
        let mut fb = FuncBuilder::internal("grows", &[], Unit);
        let mut b = fb.block();
        for _ in 0..20 {
            b = fb.call(b, Callee::Func(small), vec![], None);
        }
        for k in 0..9_969 {
            b = fb.call(b, Callee::Extern(sink), vec![int(k, I64)], None);
        }
        fb.ret(b, Operand::Const(Const::Unit, Unit));
        pb.add(fb.finish())
    };
    main_calling(&mut pb, &[big, fits, grows]);
    let p = pb.finish();
    let caps = Caps::of(&p);
    assert!(caps.too_large(big.0 as usize));
    assert!(!caps.too_large(fits.0 as usize));
    assert!(
        caps.too_large(grows.0 as usize),
        "{}",
        caps.grown[grows.0 as usize]
    );
    let ir = crate::emit_ir(&p, "x86_64-unknown-linux-gnu").unwrap();
    let defines = |name: &str, attrs: &str| {
        ir.contains(&format!("define internal void @\"{name}\"() #0{attrs} {{"))
    };
    assert!(defines("big", " noinline"));
    assert!(defines("fits", ""));
    assert!(defines("grows", " noinline"));
    // The calls of a `noinline` function need no call-site attribute.
    assert!(ir.contains("call void @\"big\"()\n"));
    // `velt_main` (4 statements) can take `fits` in.
    assert!(ir.contains("call void @\"fits\"()\n"));
}

/// Small callees and std's `@inline` functions are inlined into a caller at the limit all
/// the same; a callee past the limit is not.
#[test]
fn small_and_marked_callees_are_inlined_into_large_callers() {
    let mut pb = ProgramBuilder::new();
    let sink = pb.ext("sink", &[I64], Unit, false);
    let small = calling(&mut pb, "small", sink, SMALL as usize);
    let medium = calling(&mut pb, "medium", sink, SMALL as usize + 1);
    let marked = calling(&mut pb, "marked", sink, 500);
    let caller = {
        let mut fb = FuncBuilder::internal("caller", &[], Unit);
        let mut b = fb.block();
        for k in 0..CALLER_LIMIT as i128 {
            b = fb.call(b, Callee::Extern(sink), vec![int(k, I64)], None);
        }
        for f in [small, medium, marked] {
            b = fb.call(b, Callee::Func(f), vec![], None);
        }
        fb.ret(b, Operand::Const(Const::Unit, Unit));
        pb.add(fb.finish())
    };
    main_calling(&mut pb, &[caller]);
    let mut p = pb.finish();
    p.funcs[marked.0 as usize].inline = Inline::Always;
    let ir = crate::emit_ir(&p, "x86_64-unknown-linux-gnu").unwrap();
    assert!(ir.contains("call void @\"small\"()\n"));
    assert!(ir.contains("call void @\"medium\"() noinline"));
    assert!(ir.contains("call void @\"marked\"()\n"));
    let caps = Caps::of(&p);
    assert_eq!(caps.kept(caller.0 as usize).len(), 1);
}

/// However the calls are arranged (chains of single calls, fans, shared callees), no function
/// grows past the caller limit plus its small callees, and every call either fits or is kept.
#[test]
fn no_function_grows_past_the_limit() {
    let mut pb = ProgramBuilder::new();
    let sink = pb.ext("sink", &[I64], Unit, false);
    // A chain: each link calls the next, 1,500 statements each.
    let mut next: Option<FuncId> = None;
    let mut chain = vec![];
    for i in 0..30 {
        let mut fb = FuncBuilder::internal(&format!("link{i}"), &[], Unit);
        let mut b = fb.block();
        if let Some(n) = next {
            b = fb.call(b, Callee::Func(n), vec![], None);
        }
        for k in 0..1_500 {
            b = fb.call(b, Callee::Extern(sink), vec![int(k, I64)], None);
        }
        fb.ret(b, Operand::Const(Const::Unit, Unit));
        let f = pb.add(fb.finish());
        chain.push(f);
        next = Some(f);
    }
    // Fans of medium callees, called from several places.
    let leaves: Vec<FuncId> = (0..50)
        .map(|i| calling(&mut pb, &format!("leaf{i}"), sink, 300 + 37 * i))
        .collect();
    let fans: Vec<FuncId> = (0..10)
        .map(|i| {
            let mut fb = FuncBuilder::internal(&format!("fan{i}"), &[], Unit);
            let mut b = fb.block();
            for &l in leaves.iter().skip(i * 3).take(20) {
                b = fb.call(b, Callee::Func(l), vec![], None);
            }
            fb.ret(b, Operand::Const(Const::Unit, Unit));
            pb.add(fb.finish())
        })
        .collect();
    let mut all = chain.clone();
    all.extend(&fans);
    main_calling(&mut pb, &all);
    let p = pb.finish();
    let caps = Caps::of(&p);
    for (f, func) in p.funcs.iter().enumerate() {
        let calls = func.blocks.len() as u64;
        assert!(
            caps.grown[f] <= CALLER_LIMIT.max(crate::inlining::cost(func)) + SMALL * calls,
            "{} grows to {}",
            func.symbol,
            caps.grown[f]
        );
    }
}

/// The caps mirror velt_opt's inliner: the same caller limit and small-callee size.
#[test]
fn limits_are_velt_opts() {
    assert_eq!(CALLER_LIMIT, velt_opt::INLINE_CALLER_LIMIT);
    assert_eq!(SMALL, velt_opt::INLINE_SMALL_COST);
}

/// Calls LLVM leaves alone take no room under the limit: three calls of a 3,000-statement
/// function called from two sites, then the only call of a 6,000-statement one. Counting the
/// first three (as the caps first did) kept the last call out; LLVM inlines it, so it fits.
#[test]
fn calls_llvm_keeps_take_no_room() {
    let mut pb = ProgramBuilder::new();
    let sink = pb.ext("sink", &[I64], Unit, false);
    let shared = calling(&mut pb, "shared", sink, 3_000);
    let once = calling(&mut pb, "once", sink, 6_000);
    let main = main_calling(&mut pb, &[shared, shared, shared, once]);
    // A second caller of `shared` elsewhere: still several sites.
    main_calling_named(&mut pb, "other", &[shared]);
    let p = pb.finish();
    let caps = Caps::of(&p);
    assert!(
        caps.kept(main.0 as usize).is_empty(),
        "{:?}",
        caps.kept(main.0 as usize)
    );
    // The same call stays capped once another unit refers to `once`: then it is no single
    // local call, so LLVM leaves it, and it takes no room either.
    let mut shared_funcs = vec![false; p.funcs.len()];
    shared_funcs[once.0 as usize] = true;
    let caps = Caps::of_units(&p, &shared_funcs);
    assert!(caps.kept(main.0 as usize).is_empty());
    // Only the single local call counts: with `once` grown past the room left, it is kept.
    let mut pb = ProgramBuilder::new();
    let sink = pb.ext("sink", &[I64], Unit, false);
    let first = calling(&mut pb, "first", sink, 6_000);
    let second = calling(&mut pb, "second", sink, 6_000);
    let main = main_calling(&mut pb, &[first, second]);
    let caps = Caps::of(&pb.finish());
    assert_eq!(caps.kept(main.0 as usize), [1]);
}

/// An internal function `name` calling each of `callees` once.
fn main_calling_named(pb: &mut ProgramBuilder, name: &str, callees: &[FuncId]) -> FuncId {
    let mut fb = FuncBuilder::internal(name, &[], Unit);
    let mut b = fb.block();
    for &f in callees {
        b = fb.call(b, Callee::Func(f), vec![], None);
    }
    fb.ret(b, Operand::Const(Const::Unit, Unit));
    pb.add(fb.finish())
}
