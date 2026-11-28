//! Unit tests for string views (`views.rs`) on hand-built VIR shaped like a parser that consumes
//! its input from the front: `let rest = text; while (…) rest = rest.slice(1);`, then whatever
//! the case does with `rest` before its drop.

use super::*;
use crate::testkit::builder::*;
use crate::testkit::validate::assert_valid;
use velt_vir::vir::{AggId, Const, FuncId};

/// What happens to the family between the loop and the final drop (or inside the loop).
#[derive(Clone, Copy, PartialEq, Debug)]
enum Case {
    /// Nothing: the text is read, then `rest` is dropped.
    Nothing,
    /// A data word of `rest` returned.
    Returned,
    /// `rest` stored into the object.
    Stored,
    /// `rest` cloned into the object's string field.
    ClonedIntoMemory,
    /// `rest` cloned into another local (a bitwise copy of a view), which is then stored.
    ClonedThenStored,
    /// `rest` cloned into another local that is only read.
    ClonedThenRead,
    /// A slice of `rest` stored into the object.
    SliceStored,
    /// A slice of `rest` by std's `sliceWide` in its object (non-ASCII text), only read, then
    /// dropped: it joins the family as the runtime's byte slice does.
    WideSliceRead,
    /// `rest`'s address passed to a runtime function that may keep it.
    UnknownCall,
    /// A read-only Velt function's `u64` result of `rest` (its first word, as `strWord0` gives
    /// it), rebuilt into a string with `rest`'s second word and stored: it is a data word.
    U64ResultRebuiltAndStored,
    /// A read-only runtime call's result of `rest` (a comparison, widened to `u64`) rebuilt and
    /// stored the same way: a runtime function returns no address as an integer, so this is no
    /// view.
    RuntimeU64RebuiltAndStored,
    /// The Velt function's `u64` result only compared.
    U64ResultCompared,
    /// The object field the source is read from written inside the loop.
    SourceStoredInLoop,
    /// A call that may write anything inside the loop.
    UnknownCallInLoop,
    /// The parameter itself cloned (a string the caller holds), and a call inside the loop given
    /// only the address of a local, from a register assigned that address twice (before the loop
    /// and in it): the call cannot reach the string through its arguments, and the caller keeps
    /// it put during this call (semantics.md, "Borrows through counted objects").
    LocalAddressCallInLoop,
    /// As `LocalAddressCallInLoop`, but the call is also given the parameter: it may change the
    /// string through it.
    CallerPointerCallInLoop,
}

struct Kit {
    pb: ProgramBuilder,
    obj: AggId,
    clone: ExternId,
    drop: ExternId,
    slice: ExternId,
    /// std's `slice` of non-ASCII text in its object (`slice_entry`).
    wide: ExternId,
    hash: ExternId,
    word_of: FuncId,
    other: ExternId,
}

fn kit() -> Kit {
    let mut pb = ProgramBuilder::new();
    let obj = pb.agg(
        "Parser object",
        56,
        8,
        &[(Ty::Agg(STR_AGG), 0), (Ty::Agg(STR_AGG), 24), (Ty::U64, 48)],
    );
    let clone = pb.ext(STR_CLONE, &[Ty::Ptr, Ty::Ptr], Ty::Unit, false);
    let drop = pb.ext(STR_DROP, &[Ty::Ptr], Ty::Unit, false);
    let slice = pb.ext(
        STR_BYTE_SLICE,
        &[Ty::Ptr, Ty::U64, Ty::U64, Ty::U64, Ty::Ptr],
        Ty::Unit,
        false,
    );
    let wide = pb.ext(
        &slice_entry(),
        &[Ty::Ptr, Ty::I64, Ty::I64, Ty::Ptr],
        Ty::Unit,
        false,
    );
    let hash = pb.ext("velt_rt_str_cmp", &[Ty::Ptr, Ty::Ptr], Ty::I32, false);
    // `word_of(s)`: the first word of the string at `s` (`strWord0`), a read-only Velt function.
    let mut wb = FuncBuilder::internal("word_of", &[Ty::Ptr], Ty::U64);
    let (sp, w) = (wb.param(0), wb.local(Ty::U64));
    let b0 = wb.block();
    let first = Place {
        local: sp,
        proj: vec![Proj::Deref(Ty::Agg(STR_AGG)), Proj::Field(0)],
    };
    wb.assign(b0, w, Rvalue::Use(copy_place(first)));
    wb.ret(b0, copy_local(w));
    let word_of = pb.add(wb.finish());
    let other = pb.ext("velt_rt_str_append", &[Ty::Ptr, Ty::Ptr], Ty::Unit, false);
    Kit {
        pb,
        obj,
        clone,
        drop,
        slice,
        wide,
        hash,
        word_of,
        other,
    }
}

/// `(*p as obj).n`
fn field_of(p: Local, obj: AggId, n: u32) -> Place {
    Place {
        local: p,
        proj: vec![Proj::Deref(Ty::Agg(obj)), Proj::Field(n)],
    }
}

/// `parse(o)`: `rest = clone((*o).src)`, then a loop that reads the first byte of `rest` and
/// slices it until it is empty, then `case`, then `rest`'s drop. Returns the program.
fn program(case: Case) -> Program {
    let mut k = kit();
    let ret = if case == Case::Returned {
        Ty::U64
    } else {
        Ty::Unit
    };
    let mut fb = FuncBuilder::internal("parse", &[Ty::Ptr], ret);
    let o = fb.param(0);
    let (rest, tmp, other_str) = (
        fb.local(Ty::Agg(STR_AGG)),
        fb.local(Ty::Agg(STR_AGG)),
        fb.local(Ty::Agg(STR_AGG)),
    );
    let (src, p_rest, p_tmp, p_other, p_field) = (
        fb.local(Ty::Ptr),
        fb.local(Ty::Ptr),
        fb.local(Ty::Ptr),
        fb.local(Ty::Ptr),
        fb.local(Ty::Ptr),
    );
    let (len, more, w0, dp, byte, h, flag) = (
        fb.local(Ty::U64),
        fb.local(Ty::Bool),
        fb.local(Ty::U64),
        fb.local(Ty::Ptr),
        fb.local(Ty::U8),
        fb.local(Ty::U64),
        fb.local(Ty::Bool),
    );
    let entry = fb.block();
    fb.assign(entry, src, Rvalue::AddrOf(field_of(o, k.obj, 0)));
    fb.assign(entry, p_rest, Rvalue::AddrOf(Place::local(rest)));
    fb.assign(entry, p_tmp, Rvalue::AddrOf(Place::local(tmp)));
    fb.assign(entry, p_other, Rvalue::AddrOf(Place::local(other_str)));
    fb.assign(entry, p_field, Rvalue::AddrOf(field_of(o, k.obj, 1)));
    // The string cloned: the object's field, or for a call case, the parameter itself (a string
    // the caller holds).
    let from = if matches!(
        case,
        Case::LocalAddressCallInLoop | Case::CallerPointerCallInLoop
    ) {
        o
    } else {
        src
    };
    let head = fb.call(
        entry,
        Callee::Extern(k.clone),
        vec![copy_local(from), copy_local(p_rest)],
        None,
    );
    // head: while (rest's byte length > 0)
    fb.assign(head, len, Rvalue::Use(copy_place(field(rest, 1))));
    fb.assign(
        head,
        len,
        bin(BinOp::BitAnd, copy_local(len), int(0xffff_ffff, Ty::U64)),
    );
    fb.assign(head, more, bin(BinOp::Gt, copy_local(len), int(0, Ty::U64)));
    let (body, exit) = (fb.block(), fb.block());
    fb.branch(head, more, body, exit);
    // body: read the first byte, then rest = rest.slice(1)
    fb.assign(body, w0, Rvalue::Use(copy_place(field(rest, 0))));
    fb.assign(body, dp, Rvalue::Cast(copy_local(w0), Ty::Ptr));
    fb.assign(body, byte, Rvalue::Use(copy_place(deref(dp, Ty::U8))));
    let mut at = body;
    match case {
        Case::SourceStoredInLoop => {
            let mut word = field_of(o, k.obj, 0);
            word.proj.push(Proj::Field(0));
            fb.push(at, Stmt::Assign(word, Rvalue::Use(int(0, Ty::U64))));
        }
        Case::LocalAddressCallInLoop | Case::CallerPointerCallInLoop => {
            let (scratch, q) = (fb.local(Ty::U64), fb.local(Ty::Ptr));
            fb.assign(entry, q, Rvalue::AddrOf(Place::local(scratch)));
            fb.assign(at, q, Rvalue::AddrOf(Place::local(scratch)));
            let second = if case == Case::CallerPointerCallInLoop {
                o
            } else {
                q
            };
            at = fb.call(
                at,
                Callee::Extern(k.other),
                vec![copy_local(q), copy_local(second)],
                None,
            );
        }
        Case::UnknownCallInLoop => {
            at = fb.call(
                at,
                Callee::Extern(k.other),
                vec![copy_local(p_field), copy_local(p_field)],
                None,
            );
        }
        _ => {}
    }
    let sliced = fb.call(
        at,
        Callee::Extern(k.slice),
        vec![
            copy_local(p_rest),
            int(1, Ty::U64),
            copy_local(len),
            int(i128::from(u64::MAX), Ty::U64),
            copy_local(p_tmp),
        ],
        None,
    );
    let dropped = fb.call(
        sliced,
        Callee::Extern(k.drop),
        vec![copy_local(p_rest)],
        None,
    );
    fb.assign(dropped, rest, Rvalue::Use(copy_local(tmp)));
    fb.goto(dropped, head);
    // exit: the case, then the drop of rest.
    let mut at = exit;
    match case {
        Case::Stored => fb.push(
            at,
            Stmt::Assign(field_of(o, k.obj, 1), Rvalue::Use(copy_local(rest))),
        ),
        Case::ClonedIntoMemory => {
            at = fb.call(
                at,
                Callee::Extern(k.clone),
                vec![copy_local(p_rest), copy_local(p_field)],
                None,
            );
        }
        Case::ClonedThenStored | Case::ClonedThenRead => {
            at = fb.call(
                at,
                Callee::Extern(k.clone),
                vec![copy_local(p_rest), copy_local(p_other)],
                None,
            );
            if case == Case::ClonedThenStored {
                fb.push(
                    at,
                    Stmt::Assign(field_of(o, k.obj, 1), Rvalue::Use(copy_local(other_str))),
                );
            } else {
                fb.assign(at, w0, Rvalue::Use(copy_place(field(other_str, 0))));
                at = fb.call(at, Callee::Extern(k.drop), vec![copy_local(p_other)], None);
            }
        }
        Case::SliceStored => {
            at = fb.call(
                at,
                Callee::Extern(k.wide),
                vec![
                    copy_local(p_rest),
                    int(0, Ty::I64),
                    int(3, Ty::I64),
                    copy_local(p_tmp),
                ],
                None,
            );
            fb.push(
                at,
                Stmt::Assign(field_of(o, k.obj, 1), Rvalue::Use(copy_local(tmp))),
            );
        }
        Case::WideSliceRead => {
            at = fb.call(
                at,
                Callee::Extern(k.wide),
                vec![
                    copy_local(p_rest),
                    int(0, Ty::I64),
                    int(3, Ty::I64),
                    copy_local(p_tmp),
                ],
                None,
            );
            fb.assign(at, w0, Rvalue::Use(copy_place(field(tmp, 0))));
            at = fb.call(at, Callee::Extern(k.drop), vec![copy_local(p_tmp)], None);
        }
        Case::UnknownCall => {
            at = fb.call(
                at,
                Callee::Extern(k.other),
                vec![copy_local(p_rest), copy_local(p_field)],
                None,
            );
        }
        Case::U64ResultRebuiltAndStored
        | Case::RuntimeU64RebuiltAndStored
        | Case::U64ResultCompared => {
            if case == Case::RuntimeU64RebuiltAndStored {
                let c = fb.local(Ty::I32);
                let args = vec![copy_local(p_rest), copy_local(p_rest)];
                at = fb.call(at, Callee::Extern(k.hash), args, Some(c));
                fb.assign(at, h, Rvalue::Cast(copy_local(c), Ty::U64));
            } else {
                at = fb.call(at, Callee::Func(k.word_of), vec![copy_local(p_rest)], Some(h));
            }
            if case != Case::U64ResultCompared {
                let words = vec![copy_local(h), copy_place(field(rest, 1)), int(0, Ty::U64)];
                fb.assign(at, other_str, Rvalue::Aggregate(STR_AGG, words));
                fb.push(
                    at,
                    Stmt::Assign(field_of(o, k.obj, 1), Rvalue::Use(copy_local(other_str))),
                );
            } else {
                fb.assign(at, flag, bin(BinOp::Eq, copy_local(h), int(7, Ty::U64)));
            }
        }
        _ => {}
    }
    let done = fb.call(at, Callee::Extern(k.drop), vec![copy_local(p_rest)], None);
    let value = if case == Case::Returned {
        copy_place(field(rest, 0))
    } else {
        Operand::Const(Const::Unit, Ty::Unit)
    };
    fb.ret(done, value);
    k.pb.add(fb.finish());
    k.pb.finish()
}

/// Run the views mode on the last function; returns whether it demoted the clone.
fn views(mut p: Program) -> bool {
    let rt = Runtime::find(&p);
    let last = p.funcs.len() - 1;
    let aggs = p.aggs.clone();
    let changed = run_views(&aggs, &rt, &mut p.funcs[last]);
    assert_valid(&p);
    let f = &p.funcs[last];
    // A demotion leaves the clone in a block of its own and the entry ending in a branch.
    let entry_branches = matches!(f.blocks[0].term, Terminator::Branch { .. });
    assert_eq!(changed, entry_branches, "a change is exactly a demotion");
    // Drops and slices stay: a member may hold an owned string.
    let calls = |id| {
        (f.blocks.iter())
            .filter(|b| matches!(&b.term, Terminator::Call { callee, .. } if rt.is(callee, id)))
            .count()
    };
    assert!(calls(rt.drop) >= 2 && calls(rt.byte_slice) >= 1 && calls(rt.clone) >= 1);
    changed
}

#[track_caller]
fn assert_demoted(case: Case) {
    assert!(views(program(case)), "{case:?} should be demoted");
}

#[track_caller]
fn assert_kept(case: Case) {
    assert!(!views(program(case)), "{case:?} should keep its clone");
}

#[test]
fn a_family_that_only_slices_and_reads_is_demoted() {
    assert_demoted(Case::Nothing);
    // Also when it slices with std's `slice` of non-ASCII text (the object's entry point).
    assert_demoted(Case::WideSliceRead);
}

#[test]
fn the_demotion_tests_the_form_and_keeps_the_clone_for_other_strings() {
    let mut p = program(Case::Nothing);
    let rt = Runtime::find(&p);
    let aggs = p.aggs.clone();
    let last = p.funcs.len() - 1;
    assert!(run_views(&aggs, &rt, &mut p.funcs[last]));
    let f = &p.funcs[last];
    let Terminator::Branch { then, els, .. } = &f.blocks[0].term else {
        panic!("the entry ends in the form test");
    };
    // The view: the source's first word, its second, and a zero form word, then the loop.
    let view = &f.blocks[then.0 as usize];
    assert_eq!(view.stmts.len(), 3);
    assert!(matches!(
        &view.stmts[2],
        Stmt::Assign(_, Rvalue::Use(Operand::Const(Const::Int(0), Ty::U64)))
    ));
    // The other way: the clone as it was, to the same block.
    let clone = &f.blocks[els.0 as usize];
    assert!(matches!(&clone.term, Terminator::Call { callee, .. } if rt.is(callee, rt.clone)));
    let (Terminator::Goto(a), Terminator::Call { next: b, .. }) = (&view.term, &clone.term) else {
        panic!("both ways continue");
    };
    assert_eq!(a, b);
}

#[test]
fn the_copy_mode_is_unchanged_by_slices() {
    // The copy mode still gives up a family that slices (the slice writes its output).
    let mut p = program(Case::Nothing);
    let rt = Runtime::find(&p);
    let aggs = p.aggs.clone();
    let last = p.funcs.len() - 1;
    assert!(!run(&aggs, &rt, &mut p.funcs[last]));
}

#[test]
fn every_way_out_keeps_the_clone() {
    for case in [
        Case::Returned,
        Case::Stored,
        Case::ClonedIntoMemory,
        Case::SliceStored,
        Case::UnknownCall,
    ] {
        assert_kept(case);
    }
}

#[test]
fn a_clone_of_a_member_is_a_member() {
    // Its copy may be a view: storing it lets the view out, reading it does not.
    assert_kept(Case::ClonedThenStored);
    assert_demoted(Case::ClonedThenRead);
}

#[test]
fn a_u64_result_of_a_read_only_call_may_carry_a_data_word() {
    assert_kept(Case::U64ResultRebuiltAndStored);
    assert_demoted(Case::U64ResultCompared);
    assert_demoted(Case::RuntimeU64RebuiltAndStored);
}

#[test]
fn a_source_that_may_change_during_the_loop_keeps_the_clone() {
    assert_kept(Case::SourceStoredInLoop);
    assert_kept(Case::UnknownCallInLoop);
}

#[test]
fn a_call_given_only_a_local_address_leaves_the_source_alone() {
    assert_demoted(Case::LocalAddressCallInLoop);
    assert_kept(Case::CallerPointerCallInLoop);
}
