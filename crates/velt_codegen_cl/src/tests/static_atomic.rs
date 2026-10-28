//! A static atomic's word (`StaticData::mutable`) in a `velt dev` session: every version uses
//! the session's one word per constant, so a swap keeps its value.

use velt_vir::atomic::{AtomicCall, Order, Rmw};
use velt_vir::vir::{Callee, Const, Operand, Place, Program, Rvalue, StaticData, Terminator};

use super::jit::{call_capturing, stub_symbols};
use super::{copy_local, FuncBuilder, ProgramBuilder};
use crate::{DevSession, Reload};

/// A `velt dev` program with a static atomic (id 77, starting at 1): `bump` adds 1 to it, and
/// `answer` returns its value plus `value`; `main` calls both and prints the answer.
fn dev_program(value: i64, helper: bool) -> Program {
    use velt_vir::vir::{BinOp, Ty::*};
    let (mut pb, rt) = ProgramBuilder::new();
    let add = AtomicCall::Rmw {
        op: Rmw::Add,
        bits: 64,
        order: Order::SeqCst,
    };
    let load = AtomicCall::Load {
        bits: 64,
        order: Order::SeqCst,
    };
    let add = pb.ext(&add.symbol(), &[Ptr, U64], U64, false);
    let load = pb.ext(&load.symbol(), &[Ptr], U64, false);
    pb.p.statics.push(StaticData {
        bytes: 1u64.to_le_bytes().to_vec(),
        align: 8,
        relocs: vec![],
        mutable: Some(77),
    });
    let word = Operand::Const(Const::Static(velt_vir::vir::StaticId(0)), Ptr);
    // A function inserted first in the second version, shifting every `FuncId`.
    if helper {
        let mut fb = FuncBuilder::internal("helper", &[], I64);
        let b = fb.block();
        fb.term(b, Terminator::Return(super::int(1, I64)));
        pb.add(fb.finish());
    }
    let unit = || Terminator::Return(Operand::Const(Const::Unit, Unit));
    let mut fb = FuncBuilder::internal("bump", &[], Unit);
    let old = fb.local(U64);
    let b = fb.block();
    let args = vec![word.clone(), super::int(1, U64)];
    let b = fb.call(b, Callee::Extern(add), args, Some(Place::local(old)));
    fb.term(b, unit());
    let bump = pb.add(fb.finish());
    let mut fb = FuncBuilder::internal("answer", &[], I64);
    let (w, r) = (fb.local(U64), fb.local(I64));
    let b = fb.block();
    let b = fb.call(b, Callee::Extern(load), vec![word], Some(Place::local(w)));
    fb.assign(b, r, Rvalue::Cast(copy_local(w), I64));
    let sum = Rvalue::Binary(BinOp::Add, copy_local(r), super::int(value.into(), I64));
    fb.assign(b, r, sum);
    fb.term(b, Terminator::Return(copy_local(r)));
    let answer = pb.add(fb.finish());
    let mut fb = FuncBuilder::internal("main", &[], Unit);
    let got = fb.local(I64);
    let b = fb.block();
    let b = fb.call(b, Callee::Func(bump), vec![], None);
    let b = fb.call(b, Callee::Func(answer), vec![], Some(Place::local(got)));
    let mut out = super::Out {
        fb: &mut fb,
        rt: &rt,
        cur: b,
    };
    out.line(copy_local(got), I64);
    let cur = out.cur;
    fb.term(cur, unit());
    let main = pb.add(fb.finish());
    let (mut fb, b) = super::main_fb();
    let cur = fb.call(b, Callee::Func(main), vec![], None);
    pb.add(super::finish_main(fb, cur, 0));
    pb.p
}

/// After `answer` alone is swapped, the first version's `bump` and the new `answer` change and
/// read the same word (3 + 200, not the new version's starting 1 + 200), also when the swap
/// shifts every function id; later versions keep it too.
#[test]
fn hot_swap_keeps_one_word_per_constant() {
    for helper in [false, true] {
        let mut session = DevSession::new(&stub_symbols());
        let loaded = session.load(&dev_program(100, false)).expect("load v1");
        assert_eq!(call_capturing(loaded.main()), (0, "102\n".to_string()));
        let outcome = session.reload(&dev_program(200, helper)).expect("reload");
        assert!(matches!(outcome, Reload::Swapped { .. }), "{outcome:?}");
        assert_eq!(call_capturing(loaded.main()), (0, "203\n".to_string()));
        for v in [300, 400, 500] {
            session.reload(&dev_program(v, helper)).expect("reload");
        }
        assert_eq!(call_capturing(loaded.main()), (0, "504\n".to_string()));
    }
}
