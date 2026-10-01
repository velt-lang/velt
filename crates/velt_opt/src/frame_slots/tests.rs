//! Unit tests for frame slot promotion on a hand-built poll function, checked against the
//! reference interpreter.

use super::*;
use crate::interp::{Interp, RecordingHost};
use crate::testkit::builder::*;
use crate::testkit::validate::assert_valid;
use velt_vir::vir::{BinOp, BlockId, Callee, Const, Program, Terminator};

/// `(*p as T).path`
fn at(p: Local, ty: Ty, path: &[u32]) -> Place {
    let mut proj = vec![Proj::Deref(ty)];
    proj.extend(path.iter().map(|n| Proj::Field(*n)));
    Place { local: p, proj }
}

/// What the test program does besides the plain slot accesses.
#[derive(Clone, Copy, PartialEq)]
enum Variant {
    Plain,
    /// The frame pointer itself is passed to a call.
    PassFrame,
    /// Slots `total` and `k` share bytes (a union layout).
    Overlap,
    /// `observe` gets a null pointer: no address of the frame leaves the function.
    Private,
}

/// State `{ tag: u32 @0, total: i64 @8, arr: { ptr, u64, u64 } @16, k: i64 @40, seen: i64 @48 }`
/// and `count$poll(state, cx)`: each poll runs `for i < 5 { total += i + k; arr.1 += 1 (through
/// a derived pointer); view.1 += 1 (total through a reinterpreted view); seen += 1;
/// observe(&seen) }`, then `k += 1`, and suspends (returns 0) until `k == 3`. `main()` polls a
/// stack state until ready and returns `total * 1000 + arr.1 * 10 + seen`.
fn program(variant: Variant) -> Program {
    let mut pb = ProgramBuilder::new();
    let arr = pb.agg("array", 24, 8, &[(Ty::Ptr, 0), (Ty::U64, 8), (Ty::U64, 16)]);
    let k_off = if variant == Variant::Overlap { 8 } else { 40 };
    let state = pb.agg(
        "count state",
        56,
        8,
        &[
            (Ty::U32, 0),
            (Ty::I64, 8),
            (Ty::Agg(arr), 16),
            (Ty::I64, k_off),
            (Ty::I64, 48),
        ],
    );
    let view = pb.agg("view", 16, 8, &[(Ty::I64, 0), (Ty::I64, 8)]);
    let observe = pb.ext("observe", &[Ty::Ptr], Ty::Unit, false);
    let s = Ty::Agg(state);

    let mut fb = FuncBuilder::internal("count$poll", &[Ty::Ptr, Ty::Ptr], Ty::U32);
    let p = fb.param(0);
    let (tag, i, c, t, q, q2) = (
        fb.local(Ty::U32),
        fb.local(Ty::I64),
        fb.local(Ty::Bool),
        fb.local(Ty::I64),
        fb.local(Ty::Ptr),
        fb.local(Ty::Ptr),
    );
    let blocks: Vec<BlockId> = (0..8).map(|_| fb.block()).collect();
    let [entry, start, head, body, after, suspend, done, other] = blocks[..] else {
        unreachable!()
    };
    fb.assign(entry, tag, Rvalue::Use(copy_place(at(p, s, &[0]))));
    fb.term(
        entry,
        Terminator::Switch {
            value: copy_local(tag),
            cases: vec![(0, start), (1, start)],
            default: other,
        },
    );
    fb.ret(other, int(0, Ty::U32));
    fb.assign(start, i, Rvalue::Use(int(0, Ty::I64)));
    fb.assign(start, q, Rvalue::AddrOf(at(p, s, &[2])));
    fb.assign(start, q2, Rvalue::AddrOf(at(p, s, &[4])));
    fb.goto(start, head);
    fb.assign(head, c, bin(BinOp::Lt, copy_local(i), int(5, Ty::I64)));
    fb.branch(head, c, body, after);
    // total += i + k
    fb.assign(
        body,
        t,
        bin(BinOp::Add, copy_local(i), copy_place(at(p, s, &[3]))),
    );
    let total = at(p, s, &[1]);
    fb.push(
        body,
        Stmt::Assign(
            total.clone(),
            bin(BinOp::Add, copy_place(total), copy_local(t)),
        ),
    );
    // arr.1 += 1 through the derived pointer
    let len = at(q, Ty::Agg(arr), &[1]);
    fb.push(
        body,
        Stmt::Assign(
            len.clone(),
            bin(BinOp::Add, copy_place(len), int(1, Ty::U64)),
        ),
    );
    // view.1 += 1: `total` through a reinterpreted view of the frame
    let v1 = at(p, Ty::Agg(view), &[1]);
    fb.push(
        body,
        Stmt::Assign(v1.clone(), bin(BinOp::Add, copy_place(v1), int(1, Ty::I64))),
    );
    let seen = at(p, s, &[4]);
    fb.push(
        body,
        Stmt::Assign(
            seen.clone(),
            bin(BinOp::Add, copy_place(seen), int(1, Ty::I64)),
        ),
    );
    fb.assign(body, i, bin(BinOp::Add, copy_local(i), int(1, Ty::I64)));
    let args = match variant {
        Variant::PassFrame => vec![copy_local(p)],
        Variant::Private => vec![int(0, Ty::Ptr)],
        Variant::Plain | Variant::Overlap => vec![copy_local(q2)],
    };
    let next = fb.call(body, Callee::Extern(observe), args, None);
    fb.goto(next, head);
    let k = at(p, s, &[3]);
    fb.push(
        after,
        Stmt::Assign(
            k.clone(),
            bin(BinOp::Add, copy_place(k.clone()), int(1, Ty::I64)),
        ),
    );
    fb.assign(after, c, bin(BinOp::Lt, copy_place(k), int(3, Ty::I64)));
    fb.branch(after, c, suspend, done);
    fb.push(
        suspend,
        Stmt::Assign(at(p, s, &[0]), Rvalue::Use(int(1, Ty::U32))),
    );
    fb.ret(suspend, int(0, Ty::U32));
    fb.push(
        done,
        Stmt::Assign(at(p, s, &[0]), Rvalue::Use(int(2, Ty::U32))),
    );
    fb.ret(done, int(1, Ty::U32));
    let mut poll = fb.finish();
    poll.is_poll = true;
    let poll = pb.add(poll);

    let mut fb = FuncBuilder::export("main", &[], Ty::I64);
    let (st, sp, r, c, x, y) = (
        fb.local(s),
        fb.local(Ty::Ptr),
        fb.local(Ty::U32),
        fb.local(Ty::Bool),
        fb.local(Ty::I64),
        fb.local(Ty::I64),
    );
    let (b, again, fin) = (fb.block(), fb.block(), fb.block());
    let null = Operand::Const(Const::Int(0), Ty::Ptr);
    fb.push(b, Stmt::Assign(field(st, 0), Rvalue::Use(int(0, Ty::U32))));
    fb.push(b, Stmt::Assign(field(st, 1), Rvalue::Use(int(0, Ty::I64))));
    let zero_arr = Rvalue::Aggregate(arr, vec![null.clone(), int(0, Ty::U64), int(0, Ty::U64)]);
    fb.push(b, Stmt::Assign(field(st, 2), zero_arr));
    fb.push(b, Stmt::Assign(field(st, 3), Rvalue::Use(int(0, Ty::I64))));
    fb.push(b, Stmt::Assign(field(st, 4), Rvalue::Use(int(0, Ty::I64))));
    fb.assign(b, sp, Rvalue::AddrOf(Place::local(st)));
    fb.goto(b, again);
    let polled = fb.call(
        again,
        Callee::Func(poll),
        vec![copy_local(sp), null],
        Some(r),
    );
    fb.assign(polled, c, bin(BinOp::Eq, copy_local(r), int(0, Ty::U32)));
    fb.branch(polled, c, again, fin);
    let mut arr_len = field(st, 2);
    arr_len.proj.push(Proj::Field(1));
    fb.assign(
        fin,
        x,
        bin(BinOp::Mul, copy_place(field(st, 1)), int(1000, Ty::I64)),
    );
    fb.assign(fin, y, Rvalue::Cast(copy_place(arr_len), Ty::I64));
    fb.assign(fin, y, bin(BinOp::Mul, copy_local(y), int(10, Ty::I64)));
    fb.assign(fin, x, bin(BinOp::Add, copy_local(x), copy_local(y)));
    fb.assign(
        fin,
        x,
        bin(BinOp::Add, copy_local(x), copy_place(field(st, 4))),
    );
    fb.ret(fin, copy_local(x));
    pb.add(fb.finish());
    pb.finish()
}

fn run_main(p: &Program) -> u64 {
    let mut interp = Interp::new(p, RecordingHost::default());
    interp.call_symbol("main", &[]).expect("program runs")
}

/// The state aggregate of the test program.
const STATE: velt_vir::vir::AggId = velt_vir::vir::AggId(2);

/// Accesses of frame field `n` (by the state type) in block `b`.
fn frame_field_uses(f: &Function, b: usize, n: u32) -> usize {
    let mut count = 0;
    let mut visit = |pl: &Place, _| {
        let by_state = pl.proj.first() == Some(&Proj::Deref(Ty::Agg(STATE)));
        count +=
            (pl.local == Local(0) && by_state && pl.proj.get(1) == Some(&Proj::Field(n))) as usize;
    };
    for s in &f.blocks[b].stmts {
        crate::visit::stmt_places(s, &mut visit);
    }
    count
}

#[test]
fn promotes_loop_slots_and_keeps_behaviour() {
    let before = program(Variant::Plain);
    let expected = run_main(&before);
    // total = 3 polls × (0+1+2+3+4 + 5k) + 15 view increments; arr.1 = 15; seen = 15.
    assert_eq!(expected, (30 + 15 + 15) * 1000 + 150 + 15);
    let mut after = before.clone();
    let aggs = after.aggs.clone();
    assert!(run(&aggs, &mut after.funcs[0]));
    assert_valid(&after);
    assert_eq!(run_main(&after), expected);
    let poll = &after.funcs[0];
    // The loop body (block 3) keeps `total` (1) only around the view access (store + reload)
    // and `seen` (4, exposed to `observe`) in memory; `k` (3) is gone from it.
    assert_eq!(frame_field_uses(poll, 3, 3), 0);
    assert_eq!(frame_field_uses(poll, 3, 1), 2);
    assert_eq!(frame_field_uses(poll, 3, 4), 2);
}

#[test]
fn leaves_frames_it_cannot_account_for_alone() {
    for variant in [Variant::PassFrame, Variant::Overlap] {
        let before = program(variant);
        let mut after = before.clone();
        let aggs = after.aggs.clone();
        run(&aggs, &mut after.funcs[0]);
        assert_valid(&after);
        assert_eq!(run_main(&after), run_main(&before));
        let poll = &after.funcs[0];
        if variant == Variant::PassFrame {
            assert_eq!(frame_field_uses(poll, 3, 3), 1);
        } else {
            // `total` and `k` overlap: neither is promoted, `arr.1` still is.
            assert_eq!(frame_field_uses(poll, 3, 1), 2);
            assert_eq!(frame_field_uses(poll, 3, 3), 1);
        }
    }
}

#[test]
fn ignores_functions_that_are_not_poll_functions() {
    // The `$poll` name alone does not make a poll function: only the lowering's marker does.
    let mut p = program(Variant::Plain);
    p.funcs[0].is_poll = false;
    let aggs = p.aggs.clone();
    assert!(!run(&aggs, &mut p.funcs[0]));
}

#[test]
fn frame_is_noalias_only_when_no_address_leaves_the_function() {
    let noalias = |variant| {
        let mut p = program(variant);
        let expected = run_main(&p);
        let aggs = p.aggs.clone();
        run(&aggs, &mut p.funcs[0]);
        assert_valid(&p);
        assert_eq!(run_main(&p), expected);
        p.funcs[0].param_attr(0).noalias
    };
    // `observe(&seen)` may keep the address; `observe(frame)` too.
    assert!(!noalias(Variant::Plain));
    assert!(!noalias(Variant::PassFrame));
    assert!(noalias(Variant::Private));
}
