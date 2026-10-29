//! Unit tests for reading loop-invariant object and header fields in front of a loop nest, on
//! hand-built VIR: a typed array's view (`view.buf.bytes.len`, `view.off`) read in an inner
//! loop that may run no iteration, inside an outer loop.

use super::*;
use crate::interp::{Interp, RecordingHost, Trap};
use crate::testkit::builder::*;
use crate::testkit::validate::assert_valid;
use velt_vir::vir::{AggId, Callee, Program};

/// What the inner loop's body does besides reading the view.
#[derive(Clone, Copy, PartialEq)]
enum Extra {
    Nothing,
    /// `(*bytes).len = i`: the loop changes the length it reads.
    StoreLength,
    /// `touch(view)`: a call, which may write or free anything.
    Call,
    /// `velt_simd_u8x16_store(...)`: a SIMD store.
    SimdStore,
    /// Reads the first element through the header's data pointer as an object, which a read
    /// in front of the loop must not follow (`(*(*bytes).data as cell object).0`).
    ThroughData,
    /// `w = view` after the reads of `w` (which start as the other view): the copy does not
    /// dominate them.
    LateCopy,
    /// `w = view` before the reads of `w`.
    EarlyCopy,
    /// The view's offset is an `Atomic`'s word (another thread may store to it).
    AtomicOffset,
}

struct Aggs {
    hdr: AggId,
    buf: AggId,
    view: AggId,
    /// The `Atomic` holding the view's offset, if it is one.
    atomic: Option<AggId>,
}

/// `(*p as view).0 as buf).0 as T[]).1`: the view's bytes' length.
fn length(a: &Aggs, p: Local) -> Place {
    Place {
        local: p,
        proj: vec![
            Proj::Deref(Ty::Agg(a.view)),
            Proj::Field(0),
            Proj::Deref(Ty::Agg(a.buf)),
            Proj::Field(0),
            Proj::Deref(Ty::Agg(a.hdr)),
            Proj::Field(1),
        ],
    }
}

/// `(*p as view).1`: the view's offset (`.1.0` in an `Atomic`).
fn offset(a: &Aggs, p: Local) -> Place {
    let mut proj = vec![Proj::Deref(Ty::Agg(a.view)), Proj::Field(1)];
    proj.extend(a.atomic.map(|_| Proj::Field(0)));
    Place { local: p, proj }
}

/// The offset of the view in local `l`.
fn offset_field(a: &Aggs, l: Local) -> Place {
    let mut place = field(l, 1);
    place.proj.extend(a.atomic.map(|_| Proj::Field(0)));
    place
}

/// `main(n, reps, null_bytes)`: `for r < reps { for i < n { s += view.buf.bytes.len +
/// view.off; extra } }`, returns `s`. The bytes have length 7 (or are null when `null_bytes`),
/// the view's offset is 3, another view's offset 5. The data pointer points at a cell holding 1.
fn program(extra: Extra) -> (Program, Aggs) {
    let mut pb = ProgramBuilder::new();
    let hdr = pb.agg("T[]", 24, 8, &[(Ty::Ptr, 0), (Ty::U64, 8), (Ty::U64, 16)]);
    let buf = pb.agg("Buf object", 8, 8, &[(Ty::Ptr, 0)]);
    let atomic = (extra == Extra::AtomicOffset).then(|| pb.agg("Atomic", 8, 8, &[(Ty::U64, 0)]));
    let off_ty = atomic.map_or(Ty::U64, Ty::Agg);
    let view = pb.agg("View object", 16, 8, &[(Ty::Ptr, 0), (off_ty, 8)]);
    let cell = pb.agg("Cell object", 8, 8, &[(Ty::U64, 0)]);
    let a = Aggs {
        hdr,
        buf,
        view,
        atomic,
    };
    let store = pb.ext(
        "velt_simd_u8x16_store",
        &[Ty::V128, Ty::Ptr, Ty::U64],
        Ty::Unit,
        false,
    );

    let mut fb = FuncBuilder::internal("touch", &[Ty::Ptr], Ty::Unit);
    let b = fb.block();
    fb.ret(b, Operand::Const(Const::Unit, Ty::Unit));
    let touch = pb.add(fb.finish());

    let mut fb = FuncBuilder::export("main", &[Ty::U64, Ty::U64, Ty::Bool], Ty::U64);
    let (n, reps, null_bytes) = (fb.param(0), fb.param(1), fb.param(2));
    let (h, bo, vo, wo, co) = (
        fb.local(Ty::Agg(hdr)),
        fb.local(Ty::Agg(buf)),
        fb.local(Ty::Agg(view)),
        fb.local(Ty::Agg(view)),
        fb.local(Ty::Agg(cell)),
    );
    let (ph, pb_, pv, pw, pc) = (
        fb.local(Ty::Ptr),
        fb.local(Ty::Ptr),
        fb.local(Ty::Ptr),
        fb.local(Ty::Ptr),
        fb.local(Ty::Ptr),
    );
    let (r, i, s, c, len, w, v) = (
        fb.local(Ty::U64),
        fb.local(Ty::U64),
        fb.local(Ty::U64),
        fb.local(Ty::Bool),
        fb.local(Ty::U64),
        fb.local(Ty::Ptr),
        fb.local(Ty::V128),
    );
    let (entry, set_null, set_bytes, start) = (fb.block(), fb.block(), fb.block(), fb.block());
    let (outer, pre, inner, body, latch, exit) = (
        fb.block(),
        fb.block(),
        fb.block(),
        fb.block(),
        fb.block(),
        fb.block(),
    );
    fb.push(
        entry,
        Stmt::Assign(field(co, 0), Rvalue::Use(int(1, Ty::U64))),
    );
    fb.assign(entry, pc, Rvalue::AddrOf(Place::local(co)));
    fb.push(
        entry,
        Stmt::Assign(field(h, 0), Rvalue::Use(copy_local(pc))),
    );
    fb.push(
        entry,
        Stmt::Assign(field(h, 1), Rvalue::Use(int(7, Ty::U64))),
    );
    fb.push(
        entry,
        Stmt::Assign(field(h, 2), Rvalue::Use(int(7, Ty::U64))),
    );
    fb.assign(entry, ph, Rvalue::AddrOf(Place::local(h)));
    fb.branch(entry, null_bytes, set_null, set_bytes);
    fb.push(
        set_null,
        Stmt::Assign(field(bo, 0), Rvalue::Use(int(0, Ty::Ptr))),
    );
    fb.goto(set_null, start);
    fb.push(
        set_bytes,
        Stmt::Assign(field(bo, 0), Rvalue::Use(copy_local(ph))),
    );
    fb.goto(set_bytes, start);
    fb.assign(start, pb_, Rvalue::AddrOf(Place::local(bo)));
    fb.push(
        start,
        Stmt::Assign(field(vo, 0), Rvalue::Use(copy_local(pb_))),
    );
    fb.push(
        start,
        Stmt::Assign(offset_field(&a, vo), Rvalue::Use(int(3, Ty::U64))),
    );
    fb.assign(start, pv, Rvalue::AddrOf(Place::local(vo)));
    fb.push(
        start,
        Stmt::Assign(field(wo, 0), Rvalue::Use(copy_local(pb_))),
    );
    fb.push(
        start,
        Stmt::Assign(offset_field(&a, wo), Rvalue::Use(int(5, Ty::U64))),
    );
    fb.assign(start, pw, Rvalue::AddrOf(Place::local(wo)));
    fb.assign(start, w, Rvalue::Use(copy_local(pw)));
    fb.assign(
        start,
        v,
        Rvalue::Use(Operand::Const(Const::Int(0), Ty::V128)),
    );
    fb.assign(start, s, Rvalue::Use(int(0, Ty::U64)));
    fb.assign(start, r, Rvalue::Use(int(0, Ty::U64)));
    fb.goto(start, outer);

    fb.assign(outer, c, bin(BinOp::Lt, copy_local(r), copy_local(reps)));
    fb.branch(outer, c, pre, exit);
    fb.assign(pre, i, Rvalue::Use(int(0, Ty::U64)));
    fb.goto(pre, inner);
    fb.assign(inner, c, bin(BinOp::Lt, copy_local(i), copy_local(n)));
    fb.branch(inner, c, body, latch);

    // The view the body reads: `pv`, or `w`, which a copy of `pv` in the loop may replace.
    let read = match extra {
        Extra::LateCopy | Extra::EarlyCopy => w,
        _ => pv,
    };
    if extra == Extra::EarlyCopy {
        fb.assign(body, w, Rvalue::Use(copy_local(pv)));
    }
    fb.assign(body, len, Rvalue::Use(copy_place(length(&a, read))));
    fb.assign(body, s, bin(BinOp::Add, copy_local(s), copy_local(len)));
    fb.assign(
        body,
        s,
        bin(BinOp::Add, copy_local(s), copy_place(offset(&a, read))),
    );
    let mut tail = body;
    match extra {
        Extra::StoreLength => {
            let at = Place {
                local: ph,
                proj: vec![Proj::Deref(Ty::Agg(hdr)), Proj::Field(1)],
            };
            fb.push(tail, Stmt::Assign(at, Rvalue::Use(copy_local(i))));
        }
        Extra::Call => {
            tail = fb.call(tail, Callee::Func(touch), vec![copy_local(pv)], None);
        }
        Extra::SimdStore => {
            tail = fb.call(
                tail,
                Callee::Extern(store),
                vec![copy_local(v), copy_local(ph), int(0, Ty::U64)],
                None,
            );
        }
        Extra::ThroughData => {
            let first = Place {
                local: ph,
                proj: vec![
                    Proj::Deref(Ty::Agg(hdr)),
                    Proj::Field(0),
                    Proj::Deref(Ty::Agg(cell)),
                    Proj::Field(0),
                ],
            };
            fb.assign(tail, s, bin(BinOp::Add, copy_local(s), copy_place(first)));
        }
        Extra::LateCopy => fb.assign(tail, w, Rvalue::Use(copy_local(pv))),
        Extra::Nothing | Extra::EarlyCopy | Extra::AtomicOffset => {}
    }
    fb.assign(tail, i, bin(BinOp::Add, copy_local(i), int(1, Ty::U64)));
    fb.goto(tail, inner);
    fb.assign(latch, r, bin(BinOp::Add, copy_local(r), int(1, Ty::U64)));
    fb.goto(latch, outer);
    fb.ret(exit, copy_local(s));
    pb.add(fb.finish());
    (pb.finish(), a)
}

fn run_main(p: &Program, n: u64, reps: u64, null_bytes: bool) -> Result<u64, Trap> {
    let mut interp = Interp::new(p, RecordingHost::default());
    interp.call_symbol("main", &[n, reps, u64::from(null_bytes)])
}

/// Run the pass on `main`; returns whether it changed it.
fn optimize(p: &mut Program) -> bool {
    let aggs = p.aggs.clone();
    let externs = p.externs.clone();
    let main = p.funcs.len() - 1;
    let changed = run(&aggs, &Kinds::of(&aggs, &externs), &mut p.funcs[main]);
    assert_valid(p);
    changed
}

/// Reads through a pointer left in blocks that are on a cycle.
fn reads_in_loops(f: &Function) -> usize {
    let preds = predecessors(f);
    let doms = Dominators::new(f, &preds);
    let loops = natural_loops(f, &preds, &doms).expect("reducible");
    let mut count = 0;
    for (bi, b) in f.blocks.iter().enumerate() {
        if !loops.iter().any(|lp| lp.body[bi]) {
            continue;
        }
        let mut note = |op: &Operand| {
            if let Operand::Copy(p) = op {
                count += usize::from(p.proj.iter().any(|x| matches!(x, Proj::Deref(_))));
            }
        };
        for s in &b.stmts {
            stmt_operands(s, &mut note);
        }
        term_operands(&b.term, &mut note);
    }
    count
}

/// Every run of the original program that does not trap gives the same result optimized. (A
/// run that reads through the null bytes traps before; a program sema accepts never does.)
fn same_behaviour(before: &Program, after: &Program) {
    for (n, reps) in [(0, 0), (0, 3), (4, 0), (4, 3), (1, 1)] {
        for null_bytes in [false, true] {
            if let Ok(want) = run_main(before, n, reps, null_bytes) {
                let got = run_main(after, n, reps, null_bytes);
                assert_eq!(Ok(want), got, "n={n} reps={reps} null={null_bytes}");
            }
        }
    }
}

#[test]
fn reads_the_view_once_in_front_of_the_loop_nest() {
    let (before, _) = program(Extra::Nothing);
    assert_eq!(run_main(&before, 4, 3, false), Ok(3 * 4 * (7 + 3)));
    assert_eq!(reads_in_loops(&before.funcs[1]), 2);
    let mut after = before.clone();
    assert!(optimize(&mut after));
    // Out of both loops, not only the inner one.
    assert_eq!(reads_in_loops(&after.funcs[1]), 0);
    same_behaviour(&before, &after);
}

#[test]
fn a_null_pointer_is_not_followed_where_the_loop_does_not_run() {
    // The bytes are null and no iteration runs: the loop never reads them, and neither may
    // the reads in front of it.
    let (before, _) = program(Extra::Nothing);
    assert_eq!(run_main(&before, 0, 3, true), Ok(0));
    assert_eq!(run_main(&before, 4, 0, true), Ok(0));
    let mut after = before.clone();
    assert!(optimize(&mut after));
    assert_eq!(run_main(&after, 0, 3, true), Ok(0));
    assert_eq!(run_main(&after, 4, 0, true), Ok(0));
}

#[test]
fn a_loop_that_changes_the_length_reads_it_in_the_loop() {
    let (before, _) = program(Extra::StoreLength);
    let mut after = before.clone();
    assert!(!optimize(&mut after));
    assert_eq!(reads_in_loops(&after.funcs[1]), 2);
    same_behaviour(&before, &after);
}

#[test]
fn a_loop_that_calls_reads_in_the_loop() {
    let (before, _) = program(Extra::Call);
    let mut after = before.clone();
    assert!(!optimize(&mut after));
    assert_eq!(reads_in_loops(&after.funcs[1]), 2);
}

#[test]
fn a_simd_store_is_a_write() {
    let (before, _) = program(Extra::SimdStore);
    let mut after = before.clone();
    assert!(!optimize(&mut after));
    assert_eq!(reads_in_loops(&after.funcs[1]), 2);
}

#[test]
fn an_array_s_data_pointer_is_not_followed() {
    let (before, _) = program(Extra::ThroughData);
    let mut after = before.clone();
    assert!(optimize(&mut after));
    // The view's two reads leave the loops, the element read stays.
    assert_eq!(reads_in_loops(&after.funcs[1]), 1);
    same_behaviour(&before, &after);
}

#[test]
fn a_copy_that_does_not_dominate_the_read_keeps_it() {
    let (before, _) = program(Extra::LateCopy);
    // The first iteration reads the other view (offset 5), the later ones `pv` (offset 3).
    assert_eq!(run_main(&before, 2, 1, false), Ok((7 + 5) + (7 + 3)));
    let mut after = before.clone();
    assert!(!optimize(&mut after));
    same_behaviour(&before, &after);
}

#[test]
fn a_copy_that_dominates_the_read_is_read_through_its_source() {
    let (before, _) = program(Extra::EarlyCopy);
    let mut after = before.clone();
    assert!(optimize(&mut after));
    assert_eq!(reads_in_loops(&after.funcs[1]), 0);
    same_behaviour(&before, &after);
}

#[test]
fn an_atomic_word_is_read_in_the_loop() {
    let (before, _) = program(Extra::AtomicOffset);
    let mut after = before.clone();
    assert!(optimize(&mut after));
    // The length leaves the loops; the offset, an `Atomic`'s word, stays.
    assert_eq!(reads_in_loops(&after.funcs[1]), 1);
    same_behaviour(&before, &after);
}

#[test]
fn only_object_fields_are_followed() {
    let mut pb = ProgramBuilder::new();
    let hdr = pb.agg("T[]", 24, 8, &[(Ty::Ptr, 0), (Ty::U64, 8), (Ty::U64, 16)]);
    let cell = pb.agg("Cell object", 8, 8, &[(Ty::U64, 0)]);
    // An object with an array header inline (field 0) and a struct holding a cell (field 1).
    let pair = pb.agg("pair", 8, 8, &[(Ty::Ptr, 0)]);
    let obj = pb.agg(
        "Holder object",
        32,
        8,
        &[(Ty::Agg(hdr), 0), (Ty::Agg(pair), 24)],
    );
    let mut fb = FuncBuilder::internal("f", &[Ty::Ptr], Ty::Unit);
    let p = fb.param(0);
    let b = fb.block();
    fb.ret(b, Operand::Const(Const::Unit, Ty::Unit));
    let func = fb.finish();
    let aggs = pb.finish().aggs;
    let kinds = Kinds::of(&aggs, &[]);
    let place = |proj: Vec<Proj>| Place { local: p, proj };
    let reads = |place: Place| chain(&aggs, &kinds, &func, &place);
    let holder = Proj::Deref(Ty::Agg(obj));
    let to_cell = Proj::Deref(Ty::Agg(cell));
    // The inline header's length: a field of the object.
    assert!(reads(place(vec![
        holder.clone(),
        Proj::Field(0),
        Proj::Field(1)
    ])));
    // A cell through the struct field: an object's reference.
    let through_struct = vec![
        holder.clone(),
        Proj::Field(1),
        Proj::Field(0),
        to_cell.clone(),
        Proj::Field(0),
    ];
    assert!(reads(place(through_struct)));
    // The inline header's data pointer points at elements: not followed.
    let through_data = vec![
        holder.clone(),
        Proj::Field(0),
        Proj::Field(0),
        to_cell.clone(),
        Proj::Field(0),
    ];
    assert!(!reads(place(through_data)));
    // A scalar view of memory, an enum view, and a read of the whole object are not reads of
    // object fields.
    assert!(!reads(place(vec![Proj::Deref(Ty::U64)])));
    assert!(!reads(place(vec![Proj::Cast(cell), Proj::Field(0)])));
    assert!(!reads(place(vec![holder])));
}
