//! Loops whose decided tests LLVM would remove anyway, so a copy only adds the check in front:
//! the pass leaves them alone. Each has a control in which the test is not implied, which the
//! pass does copy.

use super::*;

/// `f(la, lb, at, dim, i0, flag)`: two headers of lengths `la` and `lb`, then a scalar loop
/// `for (i = i0; i < m; i++)` whose body does what `body` writes in block `b` (with `i`, the
/// loop's locals and a block to go to when a test fails) and then counts. `m` is
/// `min(len(a), len(a))` read twice, as std's ASCII scan gets `min(count, bytes.length)`.
struct Loop {
    fb: FuncBuilder,
    i: Local,
    at: Local,
    /// `a`'s length, read once more in front of the loop.
    len_a: Local,
    /// `b`'s length, read in front of the loop.
    len_b: Local,
    fail: BlockId,
}

fn scalar_loop(body: impl FnOnce(&mut Loop, BlockId) -> BlockId) -> Program {
    let mut pb = ProgramBuilder::new();
    let agg = header(&mut pb);
    let mut fb = FuncBuilder::new("f", &[Ty::U64; 6], Ty::U64, Linkage::Export);
    let [la, lb, at, _dim, i0, _flag] = [0, 1, 2, 3, 4, 5].map(|i| fb.param(i));
    let (ha, hb) = (fb.local(Ty::Agg(agg)), fb.local(Ty::Agg(agg)));
    let (pa, pbl) = (fb.local(Ty::Ptr), fb.local(Ty::Ptr));
    let (i, count) = (fb.local(Ty::U64), fb.local(Ty::U64));
    let entry = fb.block();
    fb.push(
        entry,
        Stmt::Assign(field(ha, 1), Rvalue::Use(copy_local(la))),
    );
    fb.push(
        entry,
        Stmt::Assign(field(hb, 1), Rvalue::Use(copy_local(lb))),
    );
    fb.assign(entry, pa, Rvalue::AddrOf(Place::local(ha)));
    fb.assign(entry, pbl, Rvalue::AddrOf(Place::local(hb)));
    fb.assign(entry, i, Rvalue::Use(copy_local(i0)));
    fb.assign(entry, count, Rvalue::Use(int(0, Ty::U64)));
    let len_of = |fb: &mut FuncBuilder, b: BlockId, p: Local| {
        let l = fb.local(Ty::U64);
        let place = Place {
            local: p,
            proj: vec![Proj::Deref(Ty::Agg(agg)), Proj::Field(1)],
        };
        fb.assign(b, l, Rvalue::Use(Operand::Copy(place)));
        l
    };
    // m = min(r1, r2), both `a`'s length read in one block, through a diamond.
    let (r1, r2) = (len_of(&mut fb, entry, pa), len_of(&mut fb, entry, pa));
    let (len_a, len_b) = (len_of(&mut fb, entry, pa), len_of(&mut fb, entry, pbl));
    let less = fb.local(Ty::Bool);
    fb.assign(entry, less, bin(BinOp::Lt, copy_local(r1), copy_local(r2)));
    let (take1, take2, join) = (fb.block(), fb.block(), fb.block());
    fb.branch(entry, less, take1, take2);
    let (pick, m) = (fb.local(Ty::U64), fb.local(Ty::U64));
    fb.assign(take1, pick, Rvalue::Use(copy_local(r1)));
    fb.goto(take1, join);
    fb.assign(take2, pick, Rvalue::Use(copy_local(r2)));
    fb.goto(take2, join);
    fb.assign(join, m, Rvalue::Use(copy_local(pick)));
    let head = fb.block();
    fb.goto(join, head);
    let c = fb.local(Ty::Bool);
    fb.assign(head, c, bin(BinOp::Lt, copy_local(i), copy_local(m)));
    let (start, exit, fail) = (fb.block(), fb.block(), fb.block());
    fb.branch(head, c, start, exit);
    let result = |fb: &mut FuncBuilder, b: BlockId, extra: i128| {
        let r = fb.local(Ty::U64);
        fb.assign(b, r, bin(BinOp::Mul, copy_local(count), int(64, Ty::U64)));
        fb.assign(b, r, bin(BinOp::Add, copy_local(r), int(extra, Ty::U64)));
        fb.ret(b, copy_local(r));
    };
    result(&mut fb, exit, 0);
    result(&mut fb, fail, 1);
    let mut lp = Loop {
        fb,
        i,
        at,
        len_a,
        len_b,
        fail,
    };
    let end = body(&mut lp, start);
    let mut fb = lp.fb;
    let next = fb.local(Ty::U64);
    fb.assign(
        end,
        count,
        bin(BinOp::Add, copy_local(count), int(1, Ty::U64)),
    );
    fb.assign(end, next, bin(BinOp::Add, copy_local(i), int(1, Ty::U64)));
    fb.assign(end, i, Rvalue::Use(copy_local(next)));
    fb.goto(end, head);
    pb.add(fb.finish());
    pb.finish()
}

/// `ok = op(x, y)` at the end of `b`, going on where it holds and to `fail` where not.
fn check(lp: &mut Loop, b: BlockId, op: BinOp, x: Local, y: Local) -> BlockId {
    let ok = lp.fb.local(Ty::Bool);
    lp.fb.assign(b, ok, bin(op, copy_local(x), copy_local(y)));
    let yes = lp.fb.block();
    let fail = lp.fail;
    lp.fb.branch(b, ok, yes, fail);
    yes
}

/// The same with the test negated: fail where `op(x, y)` holds.
fn fail_if(lp: &mut Loop, b: BlockId, op: BinOp, x: Local, y: Local) -> BlockId {
    let bad = lp.fb.local(Ty::Bool);
    lp.fb.assign(b, bad, bin(op, copy_local(x), copy_local(y)));
    let yes = lp.fb.block();
    let fail = lp.fail;
    lp.fb.branch(b, bad, fail, yes);
    yes
}

/// The program before and after, at lengths, offsets and starts around the edges.
fn decides(p: &Program) -> bool {
    let (q, changed) = versioned(p);
    let top = u64::MAX;
    let mut grid = vec![];
    for la in [0, 1, 5, 6, 40] {
        for lb in [0, 1, 5, 40] {
            for at in [0, 1, 3, top] {
                for i0 in [0, 1, 4, top] {
                    grid.push([la, lb, at, 0, i0, 0]);
                }
            }
        }
    }
    agree(p, &q, &grid);
    changed
}

#[test]
fn an_element_check_against_the_heads_bound_by_value_is_left_to_llvm() {
    // `i < min(len(a), len(a))` in the head, `i < len(a)` in the body: the same value.
    let p = scalar_loop(|lp, b| {
        let (i, len) = (lp.i, lp.len_a);
        check(lp, b, BinOp::Lt, i, len)
    });
    assert!(!decides(&p));
    // Against `b`'s length, the head implies nothing: copied.
    let p = scalar_loop(|lp, b| {
        let (i, len) = (lp.i, lp.len_b);
        check(lp, b, BinOp::Lt, i, len)
    });
    assert!(decides(&p));
}

#[test]
fn an_element_check_a_kept_test_already_makes_is_left_to_llvm() {
    // `if (at + i >= len(b)) fail; b[at + i]`: the kept test covers the element check.
    let p = scalar_loop(|lp, b| {
        let x = lp.fb.local(Ty::U64);
        lp.fb
            .assign(b, x, bin(BinOp::Add, copy_local(lp.at), copy_local(lp.i)));
        let (lenb, x2) = (lp.len_b, lp.fb.local(Ty::U64));
        let b = fail_if(lp, b, BinOp::Ge, x, lenb);
        lp.fb
            .assign(b, x2, bin(BinOp::Add, copy_local(lp.at), copy_local(lp.i)));
        check(lp, b, BinOp::Lt, x2, lenb)
    });
    assert!(!decides(&p));
    // The kept test against `a`'s length leaves `b`'s element check to decide: copied.
    let p = scalar_loop(|lp, b| {
        let x = lp.fb.local(Ty::U64);
        lp.fb
            .assign(b, x, bin(BinOp::Add, copy_local(lp.at), copy_local(lp.i)));
        let (lena, lenb, x2) = (lp.len_a, lp.len_b, lp.fb.local(Ty::U64));
        let b = fail_if(lp, b, BinOp::Ge, x, lena);
        lp.fb
            .assign(b, x2, bin(BinOp::Add, copy_local(lp.at), copy_local(lp.i)));
        check(lp, b, BinOp::Lt, x2, lenb)
    });
    assert!(decides(&p));
}
