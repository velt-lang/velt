//! Unit tests for probe reuse on hand-built VIR shaped like an inlined `get` + `set`.

use super::*;
use crate::testkit::builder::*;
use crate::testkit::validate::assert_valid;
use velt_vir::vir::{BinOp, BlockId, Local, Proj, STR_AGG};

const FIB: i128 = 0x9e37_79b9_7f4a_7c15;

/// What goes between the two probes of [`get_then_set`].
#[derive(Clone, Copy, PartialEq)]
enum Between {
    /// Only what an inlined `get` + `(… ?? 0) + 1` + `set` puts there.
    Nothing,
    /// A store through the map pointer.
    Store,
    /// A call to a function that may write anything.
    UnknownCall,
}

/// The externs and the `lookup` instance every test program uses.
struct Kit {
    pb: ProgramBuilder,
    hash: ExternId,
    clone: ExternId,
    other: ExternId,
    lookup: FuncId,
}

fn kit() -> Kit {
    let mut pb = ProgramBuilder::new();
    let hash = pb.ext("velt_rt_str_hash", &[Ty::Ptr], Ty::U64, false);
    let clone = pb.ext("velt_rt_str_clone", &[Ty::Ptr, Ty::Ptr], Ty::Unit, false);
    let other = pb.ext("velt_rt_str_push", &[Ty::Ptr, Ty::Ptr], Ty::Unit, false);
    let mut fb = FuncBuilder::internal(
        "_V3stdP7preludeP3mapN3MapM6lookup_T15string_2c_20f64",
        &[Ty::Ptr, Ty::Ptr, Ty::U64],
        Ty::I64,
    );
    let r = fb.local(Ty::I64);
    let b = fb.block();
    fb.assign(b, r, Rvalue::Use(copy_place(deref(fb.param(0), Ty::I64))));
    fb.ret(b, copy_local(r));
    let lookup = pb.add(fb.finish());
    Kit {
        pb,
        hash,
        clone,
        other,
        lookup,
    }
}

/// `hash(k)` then `lookup(m, k, hash * FIB)` into fresh locals, continuing in the returned block.
fn probe(fb: &mut FuncBuilder, kit: &Kit, b: BlockId, m: Local, k: Local) -> (BlockId, Local) {
    let (h, p, r) = (fb.local(Ty::U64), fb.local(Ty::U64), fb.local(Ty::I64));
    let b = fb.call(b, Callee::Extern(kit.hash), vec![copy_local(k)], Some(h));
    fb.assign(b, p, bin(BinOp::Mul, copy_local(h), int(FIB, Ty::U64)));
    let args = vec![copy_local(m), copy_local(k), copy_local(p)];
    (fb.call(b, Callee::Func(kit.lookup), args, Some(r)), r)
}

/// `f(m, k, other) -> i64`: probes `k`, branches on the result like `get`'s `?? 0`, clones the
/// key (from `k`, or `other` with `different_key`) field by field into a new local as `set`
/// receives it, and probes again with the clone.
fn get_then_set(between: Between, different_key: bool) -> Program {
    let mut kit = kit();
    let mut fb = FuncBuilder::export("f", &[Ty::Ptr, Ty::Ptr, Ty::Ptr], Ty::I64);
    let (m, k, other) = (fb.param(0), fb.param(1), fb.param(2));
    let entry = fb.block();
    let (after, r1) = probe(&mut fb, &kit, entry, m, k);
    let (c, v) = (fb.local(Ty::Bool), fb.local(Ty::I64));
    let (found, missing, join) = (fb.block(), fb.block(), fb.block());
    fb.assign(after, c, bin(BinOp::Lt, copy_local(r1), int(0, Ty::I64)));
    fb.branch(after, c, missing, found);
    fb.assign(found, v, Rvalue::Use(copy_place(deref(m, Ty::I64))));
    fb.goto(found, join);
    fb.assign(missing, v, Rvalue::Use(int(0, Ty::I64)));
    fb.goto(missing, join);
    match between {
        Between::Nothing => {}
        Between::Store => fb.push(
            join,
            Stmt::Assign(deref(m, Ty::I64), Rvalue::Use(int(7, Ty::I64))),
        ),
        Between::UnknownCall => {}
    }
    let join = if between == Between::UnknownCall {
        let args = vec![copy_local(m), copy_local(k)];
        fb.call(join, Callee::Extern(kit.other), args, None)
    } else {
        join
    };
    let (z, out) = (fb.local(Ty::Agg(STR_AGG)), fb.local(Ty::Ptr));
    fb.assign(join, out, Rvalue::AddrOf(Place::local(z)));
    let src = if different_key { other } else { k };
    let args = vec![copy_local(src), copy_local(out)];
    let cloned = fb.call(join, Callee::Extern(kit.clone), args, None);
    let words: Vec<Local> = (0..3).map(|_| fb.local(Ty::U64)).collect();
    for (i, w) in words.iter().enumerate() {
        let place = Place {
            local: z,
            proj: vec![Proj::Field(i as u32)],
        };
        fb.assign(cloned, *w, Rvalue::Use(copy_place(place)));
    }
    let (key, key_ptr) = (fb.local(Ty::Agg(STR_AGG)), fb.local(Ty::Ptr));
    let fields = words.iter().map(|w| copy_local(*w)).collect();
    fb.assign(cloned, key, Rvalue::Aggregate(STR_AGG, fields));
    fb.assign(cloned, key_ptr, Rvalue::AddrOf(Place::local(key)));
    let (done, r2) = probe(&mut fb, &kit, cloned, m, key_ptr);
    let sum = fb.local(Ty::I64);
    fb.assign(done, sum, bin(BinOp::Add, copy_local(r2), copy_local(v)));
    fb.ret(done, copy_local(sum));
    kit.pb.add(fb.finish());
    kit.pb.finish()
}

/// Calls to `callee` in `f`.
fn calls_to(f: &Function, callee: &Callee) -> usize {
    f.blocks
        .iter()
        .filter(|b| matches!(&b.term, Terminator::Call { callee: c, .. } if c == callee))
        .count()
}

/// Run the pass on the last function; returns (hash calls, lookup calls) after it.
fn probes_left(mut p: Program) -> (usize, usize) {
    let probes = Probes::find(&p);
    let (hash, lookup) = (
        Callee::Extern(probes.hash.expect("hash extern")),
        Callee::Func(*probes.lookups.keys().next().expect("lookup")),
    );
    let last = p.funcs.len() - 1;
    let aggs = p.aggs.clone();
    run(&aggs, &probes, &mut p.funcs[last]);
    assert_valid(&p);
    let f = &p.funcs[last];
    (calls_to(f, &hash), calls_to(f, &lookup))
}

#[test]
fn get_then_set_of_a_cloned_key_probes_once() {
    assert_eq!(probes_left(get_then_set(Between::Nothing, false)), (1, 1));
}

#[test]
fn a_store_or_call_in_between_keeps_both_probes() {
    assert_eq!(probes_left(get_then_set(Between::Store, false)), (2, 2));
    assert_eq!(
        probes_left(get_then_set(Between::UnknownCall, false)),
        (2, 2)
    );
}

#[test]
fn a_different_key_keeps_both_probes() {
    assert_eq!(probes_left(get_then_set(Between::Nothing, true)), (2, 2));
}

#[test]
fn a_probe_on_one_branch_is_not_reused_after_the_join() {
    let mut kit = kit();
    let mut fb = FuncBuilder::export("f", &[Ty::Ptr, Ty::Ptr, Ty::Bool], Ty::I64);
    let (m, k, c) = (fb.param(0), fb.param(1), fb.param(2));
    let (entry, probed, join) = (fb.block(), fb.block(), fb.block());
    fb.branch(entry, c, probed, join);
    let (after, _) = probe(&mut fb, &kit, probed, m, k);
    fb.goto(after, join);
    let (done, r) = probe(&mut fb, &kit, join, m, k);
    fb.ret(done, copy_local(r));
    kit.pb.add(fb.finish());
    assert_eq!(probes_left(kit.pb.finish()), (2, 2));
}

/// `for (i = 0; i < n; i++) { probe(&keys[i]); probe(&keys[j]) }` with `j` = `i` or `i + 1`:
/// the key pointer is recomputed from the counter, so the second probe reuses the first only
/// for the same element.
fn loop_program(next_element: bool) -> Program {
    let mut kit = kit();
    let mut fb = FuncBuilder::export("f", &[Ty::Ptr, Ty::Ptr, Ty::U64], Ty::I64);
    let (m, keys, n) = (fb.param(0), fb.param(1), fb.param(2));
    let (i, c, acc) = (fb.local(Ty::U64), fb.local(Ty::Bool), fb.local(Ty::I64));
    let (entry, head, body, exit) = (fb.block(), fb.block(), fb.block(), fb.block());
    fb.assign(entry, i, Rvalue::Use(int(0, Ty::U64)));
    fb.assign(entry, acc, Rvalue::Use(int(0, Ty::I64)));
    fb.goto(entry, head);
    fb.assign(head, c, bin(BinOp::Lt, copy_local(i), copy_local(n)));
    fb.branch(head, c, body, exit);
    let element = |fb: &mut FuncBuilder, b: BlockId, index: Local| {
        let (off, k) = (fb.local(Ty::U64), fb.local(Ty::Ptr));
        fb.assign(b, off, bin(BinOp::Mul, copy_local(index), int(24, Ty::U64)));
        fb.assign(b, k, bin(BinOp::PtrAdd, copy_local(keys), copy_local(off)));
        k
    };
    let k1 = element(&mut fb, body, i);
    let (b, r1) = probe(&mut fb, &kit, body, m, k1);
    let j = if next_element {
        let j = fb.local(Ty::U64);
        fb.assign(b, j, bin(BinOp::Add, copy_local(i), int(1, Ty::U64)));
        j
    } else {
        i
    };
    let k2 = element(&mut fb, b, j);
    let (b, r2) = probe(&mut fb, &kit, b, m, k2);
    let (s1, s2, next) = (fb.local(Ty::I64), fb.local(Ty::I64), fb.local(Ty::U64));
    fb.assign(b, s1, bin(BinOp::Add, copy_local(acc), copy_local(r1)));
    fb.assign(b, s2, bin(BinOp::Add, copy_local(s1), copy_local(r2)));
    fb.assign(b, acc, Rvalue::Use(copy_local(s2)));
    fb.assign(b, next, bin(BinOp::Add, copy_local(i), int(1, Ty::U64)));
    fb.assign(b, i, Rvalue::Use(copy_local(next)));
    fb.goto(b, head);
    fb.ret(exit, copy_local(acc));
    kit.pb.add(fb.finish());
    kit.pb.finish()
}

#[test]
fn keys_recomputed_in_a_loop_compare_by_value() {
    assert_eq!(probes_left(loop_program(false)), (1, 1));
    assert_eq!(probes_left(loop_program(true)), (2, 2));
}

#[test]
fn the_kept_probe_feeds_the_replaced_one() {
    let mut p = get_then_set(Between::Nothing, false);
    let probes = Probes::find(&p);
    let aggs = p.aggs.clone();
    assert!(run(&aggs, &probes, &mut p.funcs[1]));
    // Running again finds nothing more to do.
    assert!(!run(&aggs, &probes, &mut p.funcs[1]));
    assert_valid(&p);
}
