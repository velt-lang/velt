//! Unit tests for borrowed string copies on hand-built VIR shaped like `const s = this.src;`
//! followed by reads of `s`'s text and its drop.

use super::*;
use crate::testkit::builder::*;
use crate::testkit::validate::assert_valid;
use velt_vir::vir::{AggId, BlockId, Const, FuncId};

/// What runs between the copy and the read of its text (or, for `AfterRead`, between the read
/// and the drop).
#[derive(Clone, Copy, PartialEq, Debug)]
enum Between {
    Nothing,
    /// A store to another field of the object holding the source.
    StoreOtherField,
    /// A store to the source field itself.
    StoreSource,
    /// A store through a pointer of unknown type (`*q = 0` as `u64`).
    StoreUnknown,
    /// A call of a runtime function that may write anything.
    UnknownExtern,
    /// A call of a Velt function.
    VeltCall,
    /// A call of a runtime function that only reads (`velt_rt_str_eq` of the copy and the source).
    ReadOnlyExtern,
    /// The copy's address stored into the object.
    AddressEscapes,
    /// The copy's address passed to a runtime function that may keep or change it.
    AddressToUnknownExtern,
    /// An unknown call on one branch only, both branches then reading the text.
    UnknownCallOnOneBranch,
    /// An unknown call after the last read, before the drop.
    UnknownCallAfterRead,
    /// The copy moved into the object (a whole-string store).
    MovedOut,
    /// The source field written in a loop that reads the copy again after it.
    StoreSourceInLoop,
}

struct Kit {
    pb: ProgramBuilder,
    obj: AggId,
    clone: ExternId,
    drop: ExternId,
    eq: ExternId,
    other: ExternId,
    callee: FuncId,
}

fn kit() -> Kit {
    let mut pb = ProgramBuilder::new();
    let obj = pb.agg(
        "Scanner object",
        32,
        8,
        &[(Ty::Agg(STR_AGG), 0), (Ty::U64, 24)],
    );
    let clone = pb.ext(STR_CLONE, &[Ty::Ptr, Ty::Ptr], Ty::Unit, false);
    let drop = pb.ext(STR_DROP, &[Ty::Ptr], Ty::Unit, false);
    let eq = pb.ext("velt_rt_str_eq", &[Ty::Ptr, Ty::Ptr], Ty::U8, false);
    let other = pb.ext("velt_rt_str_append", &[Ty::Ptr, Ty::Ptr], Ty::Unit, false);
    let mut fb = FuncBuilder::internal("g", &[Ty::Ptr], Ty::Unit);
    let b = fb.block();
    fb.push(
        b,
        Stmt::Assign(field_of(fb.param(0), obj, 1), Rvalue::Use(int(9, Ty::U64))),
    );
    fb.ret(b, Operand::Const(Const::Unit, Ty::Unit));
    let callee = pb.add(fb.finish());
    Kit {
        pb,
        obj,
        clone,
        drop,
        eq,
        other,
        callee,
    }
}

/// `(*p as obj).n`
fn field_of(p: Local, obj: AggId, n: u32) -> Place {
    Place {
        local: p,
        proj: vec![Proj::Deref(Ty::Agg(obj)), Proj::Field(n)],
    }
}

/// `s = &(*this).0; clone(s, &m); …; byte = *(m.0 as ptr); drop(&m); return byte`, with
/// `between` placed as its doc says. `gated` builds the copy and drop as lowering's
/// `str_share` / `str_drop` do (a branch on the count, the plain copy, the zeroing).
fn program(between: Between, gated: bool) -> Program {
    let mut kit = kit();
    let obj = kit.obj;
    let mut fb = FuncBuilder::export("f", &[Ty::Ptr], Ty::U8);
    let this = fb.param(0);
    let entry = fb.block();
    let (s, m, t) = (
        fb.local(Ty::Ptr),
        fb.local(Ty::Agg(STR_AGG)),
        fb.local(Ty::Ptr),
    );
    fb.assign(entry, s, Rvalue::AddrOf(field_of(this, obj, 0)));
    fb.assign(entry, t, Rvalue::AddrOf(Place::local(m)));
    let copied = if gated {
        let (w2, heap) = (fb.local(Ty::I64), fb.local(Ty::Bool));
        let w2_place = Place {
            local: s,
            proj: vec![Proj::Deref(Ty::Agg(STR_AGG)), Proj::Field(2)],
        };
        fb.assign(entry, w2, Rvalue::Cast(copy_place(w2_place), Ty::I64));
        fb.assign(entry, heap, bin(BinOp::Gt, copy_local(w2), int(0, Ty::I64)));
        let (counted, plain, join) = (fb.block(), fb.block(), fb.block());
        fb.branch(entry, heap, counted, plain);
        let after = fb.call(
            counted,
            Callee::Extern(kit.clone),
            vec![copy_local(s), copy_local(t)],
            None,
        );
        fb.goto(after, join);
        let from = Place {
            local: s,
            proj: vec![Proj::Deref(Ty::Agg(STR_AGG))],
        };
        fb.assign(plain, m, Rvalue::Use(copy_place(from)));
        fb.goto(plain, join);
        join
    } else {
        fb.call(
            entry,
            Callee::Extern(kit.clone),
            vec![copy_local(s), copy_local(t)],
            None,
        )
    };
    let mut b = copied;
    let read_text = |fb: &mut FuncBuilder, b: BlockId| {
        let (w0, dp, byte) = (fb.local(Ty::U64), fb.local(Ty::Ptr), fb.local(Ty::U8));
        fb.assign(b, w0, Rvalue::Use(copy_place(field(m, 0))));
        fb.assign(b, dp, Rvalue::Cast(copy_local(w0), Ty::Ptr));
        fb.assign(b, byte, Rvalue::Use(copy_place(deref(dp, Ty::U8))));
        byte
    };
    let unknown_call = |fb: &mut FuncBuilder, b: BlockId| {
        fb.call(
            b,
            Callee::Extern(kit.other),
            vec![copy_local(this), copy_local(this)],
            None,
        )
    };
    match between {
        Between::Nothing | Between::UnknownCallAfterRead => {}
        Between::StoreOtherField => fb.push(
            b,
            Stmt::Assign(field_of(this, obj, 1), Rvalue::Use(int(1, Ty::U64))),
        ),
        Between::StoreSource => {
            let mut w = field_of(this, obj, 0);
            w.proj.push(Proj::Field(0));
            fb.push(b, Stmt::Assign(w, Rvalue::Use(int(0, Ty::U64))));
        }
        Between::StoreUnknown => {
            let (w, q) = (fb.local(Ty::U64), fb.local(Ty::Ptr));
            fb.assign(b, w, Rvalue::Use(copy_place(field_of(this, obj, 1))));
            fb.assign(b, q, Rvalue::Cast(copy_local(w), Ty::Ptr));
            fb.push(
                b,
                Stmt::Assign(deref(q, Ty::U64), Rvalue::Use(int(0, Ty::U64))),
            );
        }
        Between::UnknownExtern => b = unknown_call(&mut fb, b),
        Between::VeltCall => {
            b = fb.call(b, Callee::Func(kit.callee), vec![copy_local(this)], None);
        }
        Between::ReadOnlyExtern => {
            let r = fb.local(Ty::U8);
            let args = vec![copy_local(t), copy_local(s)];
            b = fb.call(b, Callee::Extern(kit.eq), args, Some(r));
        }
        Between::AddressEscapes => {
            let w = fb.local(Ty::U64);
            fb.assign(b, w, Rvalue::Cast(copy_local(t), Ty::U64));
            fb.push(
                b,
                Stmt::Assign(field_of(this, obj, 1), Rvalue::Use(copy_local(w))),
            );
        }
        Between::AddressToUnknownExtern => {
            b = fb.call(
                b,
                Callee::Extern(kit.other),
                vec![copy_local(t), copy_local(s)],
                None,
            );
        }
        Between::UnknownCallOnOneBranch => {
            let c = fb.local(Ty::Bool);
            fb.assign(
                b,
                c,
                bin(
                    BinOp::Gt,
                    copy_place(field_of(this, obj, 1)),
                    int(0, Ty::U64),
                ),
            );
            let (yes, no, join) = (fb.block(), fb.block(), fb.block());
            fb.branch(b, c, yes, no);
            let after = unknown_call(&mut fb, yes);
            fb.goto(after, join);
            fb.goto(no, join);
            b = join;
        }
        Between::MovedOut => fb.push(
            b,
            Stmt::Assign(field_of(this, obj, 0), Rvalue::Use(copy_local(m))),
        ),
        Between::StoreSourceInLoop => {
            // loop: read the text; write the source; until (*this).1 == 0
            let (head, exit) = (fb.block(), fb.block());
            fb.goto(b, head);
            read_text(&mut fb, head);
            let mut w = field_of(this, obj, 0);
            w.proj.push(Proj::Field(0));
            fb.push(head, Stmt::Assign(w, Rvalue::Use(int(0, Ty::U64))));
            let c = fb.local(Ty::Bool);
            fb.assign(
                head,
                c,
                bin(
                    BinOp::Eq,
                    copy_place(field_of(this, obj, 1)),
                    int(0, Ty::U64),
                ),
            );
            fb.branch(head, c, exit, head);
            b = exit;
        }
    }
    let byte = read_text(&mut fb, b);
    if between == Between::UnknownCallAfterRead {
        b = unknown_call(&mut fb, b);
    }
    let t2 = fb.local(Ty::Ptr);
    fb.assign(b, t2, Rvalue::AddrOf(Place::local(m)));
    let done = if gated {
        let (w2, heap) = (fb.local(Ty::I64), fb.local(Ty::Bool));
        fb.assign(b, w2, Rvalue::Cast(copy_place(field(m, 2)), Ty::I64));
        fb.assign(b, heap, bin(BinOp::Gt, copy_local(w2), int(0, Ty::I64)));
        let (counted, plain, join) = (fb.block(), fb.block(), fb.block());
        fb.branch(b, heap, counted, plain);
        let after = fb.call(
            counted,
            Callee::Extern(kit.drop),
            vec![copy_local(t2)],
            None,
        );
        fb.goto(after, join);
        for k in 0..3 {
            fb.push(
                plain,
                Stmt::Assign(field(m, k), Rvalue::Use(int(0, Ty::U64))),
            );
        }
        fb.goto(plain, join);
        join
    } else {
        fb.call(b, Callee::Extern(kit.drop), vec![copy_local(t2)], None)
    };
    fb.ret(done, copy_local(byte));
    kit.pb.add(fb.finish());
    kit.pb.finish()
}

/// Run the pass on the last function; returns whether it changed and the clone and drop calls
/// left.
fn borrow(mut p: Program) -> (bool, usize) {
    let rt = Runtime::find(&p);
    let last = p.funcs.len() - 1;
    let aggs = p.aggs.clone();
    let changed = run(&aggs, &rt, &mut p.funcs[last]);
    assert_valid(&p);
    let f = &p.funcs[last];
    let left = f
        .blocks
        .iter()
        .filter(|b| {
            matches!(&b.term, Terminator::Call { callee, .. }
                if rt.is(callee, rt.clone) || rt.is(callee, rt.drop))
        })
        .count();
    (changed, left)
}

#[track_caller]
fn assert_borrowed(between: Between) {
    for gated in [false, true] {
        assert_eq!(
            borrow(program(between, gated)),
            (true, 0),
            "{between:?}, gated: {gated}"
        );
    }
}

#[track_caller]
fn assert_kept(between: Between) {
    for gated in [false, true] {
        let (changed, left) = borrow(program(between, gated));
        assert!(!changed && left == 2, "{between:?}, gated: {gated}");
    }
}

#[test]
fn a_copy_only_read_borrows_its_source() {
    assert_borrowed(Between::Nothing);
}

#[test]
fn stores_to_other_fields_of_the_object_keep_the_borrow() {
    assert_borrowed(Between::StoreOtherField);
}

#[test]
fn read_only_runtime_calls_keep_the_borrow() {
    assert_borrowed(Between::ReadOnlyExtern);
}

#[test]
fn code_after_the_last_read_does_not_matter() {
    assert_borrowed(Between::UnknownCallAfterRead);
}

#[test]
fn a_write_to_the_source_keeps_the_clone() {
    assert_kept(Between::StoreSource);
    assert_kept(Between::StoreSourceInLoop);
}

#[test]
fn a_store_that_may_alias_keeps_the_clone() {
    assert_kept(Between::StoreUnknown);
}

#[test]
fn calls_that_may_write_keep_the_clone() {
    assert_kept(Between::UnknownExtern);
    assert_kept(Between::VeltCall);
    assert_kept(Between::UnknownCallOnOneBranch);
}

#[test]
fn an_escaping_copy_keeps_the_clone() {
    assert_kept(Between::AddressEscapes);
    assert_kept(Between::AddressToUnknownExtern);
    assert_kept(Between::MovedOut);
}

#[test]
fn the_borrow_is_a_plain_copy_and_the_drop_a_zeroing() {
    let mut p = program(Between::Nothing, false);
    let rt = Runtime::find(&p);
    let aggs = p.aggs.clone();
    let f = p.funcs.last_mut().expect("f");
    assert!(run(&aggs, &rt, f));
    let text = p.to_string();
    assert!(text.contains("_2 = (*_1 as agg#0)"), "{text}");
    assert!(text.contains("_2.0 = 0_u64"), "{text}");
    assert_eq!(count_calls(p.funcs.last().expect("f")), 0, "{text}");
}

#[test]
fn a_source_of_unknown_place_is_kept_across_stores_through_pointers() {
    // `f(s: ptr)`: clone(s, &m); *(s as u64) = 0 … the store may write the source.
    let mut kit = kit();
    let mut fb = FuncBuilder::export("f", &[Ty::Ptr, Ty::Ptr], Ty::U8);
    let (s, q) = (fb.param(0), fb.param(1));
    let b = fb.block();
    let (m, t, t2) = (
        fb.local(Ty::Agg(STR_AGG)),
        fb.local(Ty::Ptr),
        fb.local(Ty::Ptr),
    );
    fb.assign(b, t, Rvalue::AddrOf(Place::local(m)));
    let b = fb.call(
        b,
        Callee::Extern(kit.clone),
        vec![copy_local(s), copy_local(t)],
        None,
    );
    fb.push(
        b,
        Stmt::Assign(field_of(q, kit.obj, 1), Rvalue::Use(int(0, Ty::U64))),
    );
    let (w0, dp, byte) = (fb.local(Ty::U64), fb.local(Ty::Ptr), fb.local(Ty::U8));
    fb.assign(b, w0, Rvalue::Use(copy_place(field(m, 0))));
    fb.assign(b, dp, Rvalue::Cast(copy_local(w0), Ty::Ptr));
    fb.assign(b, byte, Rvalue::Use(copy_place(deref(dp, Ty::U8))));
    fb.assign(b, t2, Rvalue::AddrOf(Place::local(m)));
    let b = fb.call(b, Callee::Extern(kit.drop), vec![copy_local(t2)], None);
    fb.ret(b, copy_local(byte));
    kit.pb.add(fb.finish());
    assert_eq!(borrow(kit.pb.finish()), (false, 2));
}

#[test]
fn two_clones_meeting_in_one_local_through_a_shared_register_are_kept() {
    // if c { m1 = clone(s); w = m1.0 } else { m2 = clone(s); w = m2.0 }
    // x = { w, 0, 0 }; byte = *(x.0 as ptr); drop(&x)
    let mut kit = kit();
    let obj = kit.obj;
    let mut fb = FuncBuilder::export("f", &[Ty::Ptr, Ty::Bool], Ty::U8);
    let (this, c) = (fb.param(0), fb.param(1));
    let entry = fb.block();
    let s = fb.local(Ty::Ptr);
    fb.assign(entry, s, Rvalue::AddrOf(field_of(this, obj, 0)));
    let (yes, no, join) = (fb.block(), fb.block(), fb.block());
    fb.branch(entry, c, yes, no);
    let w = fb.local(Ty::U64);
    for b in [yes, no] {
        let (m, t) = (fb.local(Ty::Agg(STR_AGG)), fb.local(Ty::Ptr));
        fb.assign(b, t, Rvalue::AddrOf(Place::local(m)));
        let after = fb.call(
            b,
            Callee::Extern(kit.clone),
            vec![copy_local(s), copy_local(t)],
            None,
        );
        fb.assign(after, w, Rvalue::Use(copy_place(field(m, 0))));
        fb.goto(after, join);
    }
    let (x, dp, byte, t2) = (
        fb.local(Ty::Agg(STR_AGG)),
        fb.local(Ty::Ptr),
        fb.local(Ty::U8),
        fb.local(Ty::Ptr),
    );
    let zero = || int(0, Ty::U64);
    fb.assign(
        join,
        x,
        Rvalue::Aggregate(STR_AGG, vec![copy_local(w), zero(), zero()]),
    );
    fb.assign(join, dp, Rvalue::Cast(copy_place(field(x, 0)), Ty::Ptr));
    fb.assign(join, byte, Rvalue::Use(copy_place(deref(dp, Ty::U8))));
    fb.assign(join, t2, Rvalue::AddrOf(Place::local(x)));
    let done = fb.call(join, Callee::Extern(kit.drop), vec![copy_local(t2)], None);
    fb.ret(done, copy_local(byte));
    kit.pb.add(fb.finish());
    assert_eq!(borrow(kit.pb.finish()), (false, 3));
}

#[test]
fn text_read_through_the_text_aggregate_is_a_byte_not_a_pointer() {
    // The byte read `*(dp as u8)` as lowering writes it: `(*dp as string text).0`.
    let mut p = program(Between::Nothing, true);
    p.aggs.push(velt_vir::vir::AggLayout {
        name: velt_vir::STR_TEXT_AGG_NAME.into(),
        size: 1,
        align: 1,
        fields: vec![(Ty::U8, 0)],
    });
    let text = AggId(p.aggs.len() as u32 - 1);
    let f = p.funcs.last_mut().expect("f");
    for s in f.blocks.iter_mut().flat_map(|b| &mut b.stmts) {
        if let Stmt::Assign(_, Rvalue::Use(Operand::Copy(pl))) = s {
            if pl.proj == [Proj::Deref(Ty::U8)] {
                pl.proj = vec![Proj::Deref(Ty::Agg(text)), Proj::Field(0)];
            }
        }
    }
    assert_eq!(borrow(p), (true, 0));
}
