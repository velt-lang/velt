//! Unit tests for versioned string views (`views_version.rs`) on hand-built VIR shaped like std's
//! `sliceOf` in a parser loop: `rest = text`, then while `rest` is not empty, read its first
//! byte and slice one unit off: inline when `rest` is static ASCII (form word 0, unit count equal
//! to byte count), through the runtime otherwise.

use super::*;
use crate::testkit::builder::*;
use crate::testkit::validate::assert_valid;
use velt_vir::vir::{AggId, BlockId};

/// What the loop does besides slicing.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Case {
    /// Nothing: the copy is proven and runs on the view path.
    Slices,
    /// On one branch the loop puts another string (a local that is not in the family) into
    /// `rest`: the proof fails and the function is left as the demotion left it.
    TakesAnother,
    /// The slice is always the runtime's (no inline static path): the proof fails.
    RuntimeOnly,
    /// `tmp` (a member: the slice's output) holds another string from before the loop, and the
    /// loop head reads it through `&tmp`, taken before the loop, before the slice writes it: the
    /// copy would read that string as static ASCII, so the proof fails.
    ReadsThroughPointer,
    /// The loop calls a read-only runtime function that may unwind (a call with a landing pad):
    /// the region is not copied.
    UnwindingCall,
    /// After the loop, `rest`'s count is read through `&rest`, taken before the loop: the copy
    /// leaves with `rest` copied out and the pointer taken again, and the function verifies.
    PointerReadAfterLoop,
    /// The loop body builds a pair field by field and reads it back: the region assigns it, and
    /// its liveness (a part written keeps the rest) has it live where the copy is entered, though
    /// nothing assigned it before the loop. The copy in is left out, and the function verifies.
    PairBuiltInLoop,
}

fn field_of(p: Local, obj: AggId, n: u32) -> Place {
    Place {
        local: p,
        proj: vec![Proj::Deref(Ty::Agg(obj)), Proj::Field(n)],
    }
}

fn program(case: Case) -> Program {
    let mut pb = ProgramBuilder::new();
    let obj = pb.agg(
        "Parser object",
        48,
        8,
        &[(Ty::Agg(STR_AGG), 0), (Ty::Agg(STR_AGG), 24)],
    );
    let pair_agg = pb.agg("Pair", 16, 8, &[(Ty::U64, 0), (Ty::U64, 8)]);
    let clone = pb.ext(STR_CLONE, &[Ty::Ptr, Ty::Ptr], Ty::Unit, false);
    let eq = pb.ext("velt_rt_str_eq", &[Ty::Ptr, Ty::Ptr], Ty::U8, false);
    let drop = pb.ext(STR_DROP, &[Ty::Ptr], Ty::Unit, false);
    let slice = pb.ext(
        STR_SLICE,
        &[Ty::Ptr, Ty::I64, Ty::I64, Ty::Ptr],
        Ty::Unit,
        false,
    );
    let mut fb = FuncBuilder::internal("parse", &[Ty::Ptr], Ty::U64);
    let o = fb.param(0);
    let (rest, tmp, other) = (
        fb.local(Ty::Agg(STR_AGG)),
        fb.local(Ty::Agg(STR_AGG)),
        fb.local(Ty::Agg(STR_AGG)),
    );
    let ptr = |fb: &mut FuncBuilder| fb.local(Ty::Ptr);
    let (src, p_rest, p_tmp) = (ptr(&mut fb), ptr(&mut fb), ptr(&mut fb));
    let u64s: Vec<Local> = (0..12).map(|_| fb.local(Ty::U64)).collect();
    let [len, w2, w1, units, bytes, start, n1, high, cw1, w0, sum, b8] = u64s[..] else {
        unreachable!()
    };
    let bools: Vec<Local> = (0..4).map(|_| fb.local(Ty::Bool)).collect();
    let [more, stat, ascii, flag] = bools[..] else {
        unreachable!()
    };
    let (dp, byte) = (fb.local(Ty::Ptr), fb.local(Ty::U8));
    let entry = fb.block();
    fb.assign(entry, src, Rvalue::AddrOf(field_of(o, obj, 0)));
    fb.assign(entry, p_rest, Rvalue::AddrOf(Place::local(rest)));
    fb.assign(entry, p_tmp, Rvalue::AddrOf(Place::local(tmp)));
    fb.assign(entry, sum, Rvalue::Use(int(0, Ty::U64)));
    if case == Case::ReadsThroughPointer {
        fb.assign(entry, tmp, Rvalue::Use(copy_place(field_of(o, obj, 1))));
    }
    let head = fb.call(
        entry,
        Callee::Extern(clone),
        vec![copy_local(src), copy_local(p_rest)],
        None,
    );
    // head: while (bytes of rest > 0)
    fb.assign(head, len, Rvalue::Use(copy_place(field(rest, 1))));
    fb.assign(
        head,
        len,
        bin(BinOp::BitAnd, copy_local(len), int(0x7fff_ffff, Ty::U64)),
    );
    fb.assign(head, more, bin(BinOp::Gt, copy_local(len), int(0, Ty::U64)));
    if case == Case::ReadsThroughPointer {
        let through = Place {
            local: p_tmp,
            proj: vec![Proj::Deref(Ty::Agg(STR_AGG)), Proj::Field(1)],
        };
        fb.assign(head, b8, Rvalue::Use(copy_place(through)));
        fb.assign(head, sum, bin(BinOp::Add, copy_local(sum), copy_local(b8)));
    }
    let (body, exit) = (fb.block(), fb.block());
    fb.branch(head, more, body, exit);
    // body: sum += first byte; then the slice's form tests.
    fb.assign(body, w0, Rvalue::Use(copy_place(field(rest, 0))));
    fb.assign(body, dp, Rvalue::Cast(copy_local(w0), Ty::Ptr));
    fb.assign(body, byte, Rvalue::Use(copy_place(deref(dp, Ty::U8))));
    fb.assign(body, b8, Rvalue::Cast(copy_local(byte), Ty::U64));
    fb.assign(body, sum, bin(BinOp::Add, copy_local(sum), copy_local(b8)));
    if case == Case::PairBuiltInLoop {
        let pair = fb.local(Ty::Agg(pair_agg));
        fb.push(
            body,
            Stmt::Assign(field(pair, 0), Rvalue::Use(copy_local(b8))),
        );
        fb.push(
            body,
            Stmt::Assign(field(pair, 1), Rvalue::Use(copy_local(len))),
        );
        fb.assign(body, b8, Rvalue::Use(copy_place(field(pair, 1))));
        fb.assign(body, sum, bin(BinOp::Add, copy_local(sum), copy_local(b8)));
    }
    let mut body = body;
    if case == Case::UnwindingCall {
        // r = velt_rt_str_eq(&rest, &rest), with a landing pad that returns.
        let (next, pad) = (fb.block(), fb.block());
        let r = fb.local(Ty::U8);
        fb.term(
            body,
            Terminator::Call {
                callee: Callee::Extern(eq),
                args: vec![copy_local(p_rest), copy_local(p_rest)],
                dest: Some(Place::local(r)),
                next,
                unwind: Some(pad),
            },
        );
        fb.ret(pad, copy_local(sum));
        body = next;
    }
    fb.assign(body, w2, Rvalue::Use(copy_place(field(rest, 2))));
    fb.assign(body, stat, bin(BinOp::Eq, copy_local(w2), int(0, Ty::U64)));
    let (test_ascii, inline, runtime, joined) = (fb.block(), fb.block(), fb.block(), fb.block());
    if case == Case::RuntimeOnly {
        fb.goto(body, runtime);
    } else {
        fb.branch(body, stat, test_ascii, runtime);
    }
    fb.assign(test_ascii, w1, Rvalue::Use(copy_place(field(rest, 1))));
    fb.assign(
        test_ascii,
        units,
        bin(BinOp::UShr, copy_local(w1), int(32, Ty::U64)),
    );
    fb.assign(
        test_ascii,
        bytes,
        bin(BinOp::BitAnd, copy_local(w1), int(0xffff_ffff, Ty::U64)),
    );
    fb.assign(
        test_ascii,
        ascii,
        bin(BinOp::Eq, copy_local(units), copy_local(bytes)),
    );
    fb.branch(test_ascii, ascii, inline, runtime);
    // inline: tmp = {w0 + 1, (n - 1) << 32 | (n - 1), 0}
    fb.assign(
        inline,
        start,
        bin(BinOp::Add, copy_local(w0), int(1, Ty::U64)),
    );
    // m = (n - 1) & 0x7fffffff, masked to 31 bits as std's `sliceOf` does: the proof only takes
    // a count it knows is that small.
    let sub = fb.local(Ty::U64);
    fb.assign(
        inline,
        sub,
        bin(BinOp::Sub, copy_local(bytes), int(1, Ty::U64)),
    );
    fb.assign(
        inline,
        n1,
        bin(BinOp::BitAnd, copy_local(sub), int(0x7fff_ffff, Ty::U64)),
    );
    fb.assign(
        inline,
        high,
        bin(BinOp::Shl, copy_local(n1), int(32, Ty::U64)),
    );
    fb.assign(
        inline,
        cw1,
        bin(BinOp::BitOr, copy_local(high), copy_local(n1)),
    );
    let words = vec![copy_local(start), copy_local(cw1), int(0, Ty::U64)];
    fb.assign(inline, tmp, Rvalue::Aggregate(STR_AGG, words));
    fb.goto(inline, joined);
    // runtime: velt_rt_str_slice(&rest, 1, MAX, &tmp)
    let after = fb.call(
        runtime,
        Callee::Extern(slice),
        vec![
            copy_local(p_rest),
            int(1, Ty::I64),
            int(i128::from(i64::MAX), Ty::I64),
            copy_local(p_tmp),
        ],
        None,
    );
    fb.goto(after, joined);
    // joined: drop(rest); rest = tmp (or, for TakesAnother, sometimes another string).
    let dropped = fb.call(joined, Callee::Extern(drop), vec![copy_local(p_rest)], None);
    if case == Case::TakesAnother {
        fb.assign(
            dropped,
            flag,
            bin(BinOp::Eq, copy_local(sum), int(7, Ty::U64)),
        );
        let (take, keep) = (fb.block(), fb.block());
        fb.branch(dropped, flag, take, keep);
        fb.assign(take, other, Rvalue::Use(copy_place(field_of(o, obj, 1))));
        fb.assign(take, rest, Rvalue::Use(copy_local(other)));
        fb.goto(take, head);
        fb.assign(keep, rest, Rvalue::Use(copy_local(tmp)));
        fb.goto(keep, head);
    } else {
        fb.assign(dropped, rest, Rvalue::Use(copy_local(tmp)));
        fb.goto(dropped, head);
    }
    // exit: drop(rest), return the sum.
    if case == Case::PointerReadAfterLoop {
        let through = Place {
            local: p_rest,
            proj: vec![Proj::Deref(Ty::Agg(STR_AGG)), Proj::Field(1)],
        };
        fb.assign(exit, b8, Rvalue::Use(copy_place(through)));
        fb.assign(exit, sum, bin(BinOp::Add, copy_local(sum), copy_local(b8)));
    }
    let done = fb.call(exit, Callee::Extern(drop), vec![copy_local(p_rest)], None);
    fb.ret(done, copy_local(sum));
    pb.add(fb.finish());
    pb.finish()
}

/// Run the views mode; returns the function, whether a runtime slice is reachable from the
/// demotion's view branch, and how many drops are.
fn run_case(case: Case) -> (Function, bool, usize) {
    let mut p = program(case);
    let rt = Runtime::find(&p);
    let aggs = p.aggs.clone();
    assert!(
        run_views(&aggs, &rt, &mut p.funcs[0]),
        "{case:?}: the family is demoted"
    );
    assert_valid(&p);
    let f = p.funcs.remove(0);
    let Terminator::Branch { then, .. } = f.blocks[0].term else {
        panic!("{case:?}: the entry ends in the demotion's form test");
    };
    let mut seen = vec![false; f.blocks.len()];
    let mut work = vec![then.0 as usize];
    let (mut slices, mut drops) = (false, 0);
    while let Some(b) = work.pop() {
        if std::mem::replace(&mut seen[b], true) {
            continue;
        }
        if let Terminator::Call { callee, .. } = &f.blocks[b].term {
            slices |= rt.is(callee, rt.slice);
            drops += usize::from(rt.is(callee, rt.drop));
        }
        work.extend(
            crate::visit::successors(&f.blocks[b].term)
                .iter()
                .map(|s: &BlockId| s.0 as usize),
        );
    }
    (f, slices, drops)
}

#[test]
fn a_loop_that_only_slices_runs_a_copy_without_runtime_calls_on_the_view_path() {
    let (_, slices, drops) = run_case(Case::Slices);
    assert!(!slices, "no runtime slice is reachable from the view");
    // The loop's drop is the copy's zeroing; only the drop after the loop, in the original code
    // the copy leaves to, is still a call (a view's drop does nothing).
    assert!(drops <= 1, "{drops} drops reachable from the view");
}

#[test]
fn another_string_in_the_family_fails_the_proof_and_leaves_the_demotion_alone() {
    let (f, slices, drops) = run_case(Case::TakesAnother);
    assert!(slices && drops >= 2, "the view path runs the original loop");
    // Nothing but the demotion's two blocks was added.
    let original = program(Case::TakesAnother).funcs.remove(0);
    assert_eq!(f.blocks.len(), original.blocks.len() + 2);
}

#[test]
fn a_runtime_slice_left_in_the_copy_fails_the_proof() {
    let (f, slices, _) = run_case(Case::RuntimeOnly);
    assert!(slices);
    let original = program(Case::RuntimeOnly).funcs.remove(0);
    assert_eq!(f.blocks.len(), original.blocks.len() + 2);
}

#[test]
fn a_member_read_through_a_pointer_from_before_the_loop_fails_the_proof() {
    let (f, slices, _) = run_case(Case::ReadsThroughPointer);
    assert!(slices, "the view path runs the original loop");
    let original = program(Case::ReadsThroughPointer).funcs.remove(0);
    assert_eq!(f.blocks.len(), original.blocks.len() + 2);
}

#[test]
fn a_loop_with_an_unwinding_call_is_not_copied() {
    let (f, slices, _) = run_case(Case::UnwindingCall);
    assert!(slices, "the view path runs the original loop");
    let original = program(Case::UnwindingCall).funcs.remove(0);
    assert_eq!(f.blocks.len(), original.blocks.len() + 2);
}

#[test]
fn a_pointer_read_after_the_loop_is_taken_again_on_the_way_out() {
    // `run_case` validates the function: a local read after the copy without a value would
    // fail the VIR verifier's definite assignment check.
    let (_, slices, _) = run_case(Case::PointerReadAfterLoop);
    assert!(!slices, "the view path runs the copy");
}

#[test]
fn a_local_the_loop_assigns_part_by_part_is_not_copied_in_unassigned() {
    // `run_case` validates the function: the copy in of the pair, unassigned before the loop,
    // would fail the VIR verifier's definite assignment check.
    let (_, slices, _) = run_case(Case::PairBuiltInLoop);
    assert!(!slices, "the view path runs the copy");
}

#[test]
fn the_versioned_function_keeps_the_original_loop_for_other_strings() {
    let (f, _, _) = run_case(Case::Slices);
    let Terminator::Branch { els, .. } = f.blocks[0].term else {
        panic!("the entry ends in the demotion's form test");
    };
    // The clone path still reaches the runtime slice.
    let rt = Runtime::find(&program(Case::Slices));
    let mut seen = vec![false; f.blocks.len()];
    let mut work = vec![els.0 as usize];
    let mut slices = false;
    while let Some(b) = work.pop() {
        if std::mem::replace(&mut seen[b], true) {
            continue;
        }
        if let Terminator::Call { callee, .. } = &f.blocks[b].term {
            slices |= rt.is(callee, rt.slice);
        }
        work.extend(
            crate::visit::successors(&f.blocks[b].term)
                .iter()
                .map(|s| s.0 as usize),
        );
    }
    assert!(slices);
}

/// `same(o)`: `rest = clone((*o).src)`, then `velt_rt_str_eq(&rest, &rest)` and `rest`'s drop:
/// a family whose live region has no form test to fold.
fn nothing_to_fold() -> Program {
    let mut pb = ProgramBuilder::new();
    let obj = pb.agg("Holder", 24, 8, &[(Ty::Agg(STR_AGG), 0)]);
    let clone = pb.ext(STR_CLONE, &[Ty::Ptr, Ty::Ptr], Ty::Unit, false);
    let eq = pb.ext("velt_rt_str_eq", &[Ty::Ptr, Ty::Ptr], Ty::U8, false);
    let drop = pb.ext(STR_DROP, &[Ty::Ptr], Ty::Unit, false);
    let slice = pb.ext(
        STR_SLICE,
        &[Ty::Ptr, Ty::I64, Ty::I64, Ty::Ptr],
        Ty::Unit,
        false,
    );
    let mut fb = FuncBuilder::internal("same", &[Ty::Ptr], Ty::U8);
    let o = fb.param(0);
    let (rest, tmp) = (fb.local(Ty::Agg(STR_AGG)), fb.local(Ty::Agg(STR_AGG)));
    let (src, p_rest, p_tmp) = (fb.local(Ty::Ptr), fb.local(Ty::Ptr), fb.local(Ty::Ptr));
    let r = fb.local(Ty::U8);
    let entry = fb.block();
    fb.assign(entry, src, Rvalue::AddrOf(field_of(o, obj, 0)));
    fb.assign(entry, p_rest, Rvalue::AddrOf(Place::local(rest)));
    fb.assign(entry, p_tmp, Rvalue::AddrOf(Place::local(tmp)));
    let compare = fb.call(
        entry,
        Callee::Extern(clone),
        vec![copy_local(src), copy_local(p_rest)],
        None,
    );
    // A slice so that the views mode runs (it needs the runtime's slice), on a path never taken.
    let (sliced, after) = (fb.block(), fb.block());
    fb.term(
        compare,
        Terminator::Call {
            callee: Callee::Extern(eq),
            args: vec![copy_local(p_rest), copy_local(p_rest)],
            dest: Some(Place::local(r)),
            next: after,
            unwind: None,
        },
    );
    let dropped = fb.call(after, Callee::Extern(drop), vec![copy_local(p_rest)], None);
    fb.ret(dropped, copy_local(r));
    let back = fb.call(
        sliced,
        Callee::Extern(slice),
        vec![
            copy_local(p_rest),
            int(1, Ty::I64),
            int(2, Ty::I64),
            copy_local(p_tmp),
        ],
        None,
    );
    fb.ret(back, copy_local(r));
    pb.add(fb.finish());
    pb.finish()
}

#[test]
fn a_region_with_nothing_to_fold_is_not_copied() {
    let mut p = nothing_to_fold();
    let rt = Runtime::find(&p);
    let aggs = p.aggs.clone();
    assert!(
        run_views(&aggs, &rt, &mut p.funcs[0]),
        "the family is demoted"
    );
    assert_valid(&p);
    // Nothing but the demotion's two blocks was added.
    let original = nothing_to_fold().funcs.remove(0);
    assert_eq!(p.funcs[0].blocks.len(), original.blocks.len() + 2);
}
