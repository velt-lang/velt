//! Unit tests for threading jumps into tag tests, on hand-built VIR shaped like an inlined
//! throwing callee's two returns joining at its caller's check of the result.

use super::*;
use crate::testkit::builder::*;
use crate::testkit::validate::assert_valid;
use velt_vir::vir::{AggId, Program};

/// What else the throwing return does after storing its tag.
#[derive(Clone, Copy, PartialEq)]
enum After {
    Nothing,
    /// Stores the error payload (field 1 of an error view of the result).
    Payload,
    /// Overwrites the whole result.
    Whole,
    /// The tag's value is read again after the test.
    TagReadLater,
}

/// `if (p) { res.0 = 1; … } else { res.0 = 0 }; t = res.0; c = t != 0; branch c, err, ok`;
/// `err` returns 1, `ok` returns 2. With `address_taken`, `&res` is taken first.
fn program(after: After, address_taken: bool) -> (Program, usize) {
    let mut pb = ProgramBuilder::new();
    let res = pb.agg("Result", 16, 8, &[(Ty::U8, 0), (Ty::I64, 8)]);
    let err_view = pb.agg("Result::Err", 16, 8, &[(Ty::U8, 0), (Ty::I64, 8)]);
    let mut fb = FuncBuilder::internal("f", &[Ty::Bool], Ty::I64);
    let p = fb.param(0);
    let r = fb.local(Ty::Agg(res));
    let (t, c, a) = (fb.local(Ty::U8), fb.local(Ty::Bool), fb.local(Ty::Ptr));
    let blocks: Vec<BlockId> = (0..6).map(|_| fb.block()).collect();
    let [entry, throws, returns, test, err, ok] = blocks[..] else {
        unreachable!()
    };
    if address_taken {
        fb.assign(entry, a, Rvalue::AddrOf(Place::local(r)));
    }
    fb.branch(entry, p, throws, returns);
    fb.push(
        throws,
        Stmt::Assign(field(r, 0), Rvalue::Use(int(1, Ty::U8))),
    );
    match after {
        After::Payload => fb.push(
            throws,
            Stmt::Assign(
                Place {
                    local: r,
                    proj: vec![Proj::Cast(err_view), Proj::Field(1)],
                },
                Rvalue::Use(int(7, Ty::I64)),
            ),
        ),
        After::Whole => fb.push(
            throws,
            Stmt::Assign(
                Place::local(r),
                Rvalue::Aggregate(res, vec![int(0, Ty::U8), int(0, Ty::I64)]),
            ),
        ),
        _ => {}
    }
    fb.goto(throws, test);
    fb.push(
        returns,
        Stmt::Assign(field(r, 0), Rvalue::Use(int(0, Ty::U8))),
    );
    fb.goto(returns, test);
    fb.assign(test, t, Rvalue::Use(copy_place(field(r, 0))));
    fb.assign(test, c, bin(BinOp::Ne, copy_local(t), int(0, Ty::U8)));
    fb.branch(test, c, err, ok);
    let x = if after == After::TagReadLater {
        let y = fb.local(Ty::I64);
        fb.assign(err, y, Rvalue::Cast(copy_local(t), Ty::I64));
        copy_local(y)
    } else {
        int(1, Ty::I64)
    };
    fb.ret(err, x);
    fb.ret(ok, int(2, Ty::I64));
    let _: AggId = res;
    pb.add(fb.finish());
    (pb.finish(), test.0 as usize)
}

fn jump(p: &Program, b: usize) -> Option<BlockId> {
    match p.funcs[0].blocks[b].term {
        Terminator::Goto(t) => Some(t),
        _ => None,
    }
}

#[test]
fn each_return_goes_where_its_tag_sends_it() {
    for after in [After::Nothing, After::Payload] {
        let (mut p, test) = program(after, false);
        assert!(run(&p.aggs.clone(), &mut p.funcs[0]));
        assert_valid(&p);
        assert_eq!(
            jump(&p, 1),
            Some(BlockId(4)),
            "the throwing return to the error path"
        );
        assert_eq!(jump(&p, 2), Some(BlockId(5)), "the Ok return past the test");
        assert_ne!(jump(&p, 1), Some(BlockId(test as u32)));
    }
}

#[test]
fn an_undecided_tag_stays_tested() {
    for (after, taken) in [
        (After::Whole, false),
        (After::TagReadLater, false),
        (After::Nothing, true),
    ] {
        let (mut p, test) = program(after, taken);
        let aggs = p.aggs.clone();
        run(&aggs, &mut p.funcs[0]);
        assert_eq!(jump(&p, 1), Some(BlockId(test as u32)));
    }
}
