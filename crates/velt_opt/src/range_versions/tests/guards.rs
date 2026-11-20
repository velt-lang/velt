//! The guards behind each decided test, one loop each that a pass without the guard would get
//! wrong: the counter assigned twice, a definition that does not dominate its use, a comparison
//! in the head before the counter's test, a memory copy into a header, an address-taken bound.
//! Each runs before and after the pass at the edges and must agree.

use super::*;

/// One `f32x4.fits` loop `f(la, at, dim, i0, x0, flag)` over an array of length `la`, counting
/// iterations, built a block at a time: `i + 16 <= dim` in the head, a step of 16.
struct Kit {
    pb: ProgramBuilder,
    fb: FuncBuilder,
    fits: Callee,
    agg: AggId,
    la: Local,
    at: Local,
    dim: Local,
    x0: Local,
    flag: Local,
    pa: Local,
    i: Local,
    count: Local,
    entry: BlockId,
    head: BlockId,
    body: BlockId,
    exit: BlockId,
    number: u64,
}

const STEP: u64 = 16;

impl Kit {
    fn new() -> Kit {
        let mut pb = ProgramBuilder::new();
        let agg = header(&mut pb);
        let fits = ext(&mut pb, Lanes::F32, "fits", &[Ty::Ptr, Ty::U64]);
        let mut fb = FuncBuilder::new("f", &[Ty::U64; 6], Ty::U64, Linkage::Export);
        let [la, at, dim, i0, x0, flag] = [0, 1, 2, 3, 4, 5].map(|i| fb.param(i));
        let ha = fb.local(Ty::Agg(agg));
        let (pa, i, count) = (fb.local(Ty::Ptr), fb.local(Ty::U64), fb.local(Ty::U64));
        let entry = fb.block();
        fb.push(
            entry,
            Stmt::Assign(field(ha, 1), Rvalue::Use(copy_local(la))),
        );
        fb.assign(entry, pa, Rvalue::AddrOf(Place::local(ha)));
        fb.assign(entry, i, Rvalue::Use(copy_local(i0)));
        fb.assign(entry, count, Rvalue::Use(int(0, Ty::U64)));
        let (head, body, exit) = (fb.block(), fb.block(), fb.block());
        Kit {
            pb,
            fb,
            fits,
            agg,
            la,
            at,
            dim,
            x0,
            flag,
            pa,
            i,
            count,
            entry,
            head,
            body,
            exit,
            number: 1,
        }
    }

    fn local(&mut self, ty: Ty) -> Local {
        self.fb.local(ty)
    }

    /// `dest = a + b` in `b`.
    fn add(&mut self, blk: BlockId, a: Operand, b: Operand) -> Local {
        let dest = self.local(Ty::U64);
        self.fb.assign(blk, dest, bin(BinOp::Add, a, b));
        dest
    }

    /// The head's test, after what `before` puts in the head; then the head branches.
    fn close_head(&mut self) {
        let t = self.add(self.head, copy_local(self.i), int(STEP.into(), Ty::U64));
        let c = self.local(Ty::Bool);
        self.fb.assign(
            self.head,
            c,
            bin(BinOp::Le, copy_local(t), copy_local(self.dim)),
        );
        self.fb.branch(self.head, c, self.body, self.exit);
    }

    /// `return count * 64 + extra` from `b`.
    fn result(&mut self, b: BlockId, extra: Operand) {
        let r = self.local(Ty::U64);
        self.fb.assign(
            b,
            r,
            bin(BinOp::Mul, copy_local(self.count), int(64, Ty::U64)),
        );
        self.fb.assign(b, r, bin(BinOp::Add, copy_local(r), extra));
        self.fb.ret(b, copy_local(r));
    }

    /// `fits(a, at)` at the end of `cur`, returning its number where it fails; the block where
    /// it held.
    fn test(&mut self, cur: BlockId, at: Local) -> BlockId {
        let r = self.local(Ty::Bool);
        let args = vec![copy_local(self.pa), copy_local(at)];
        let after = self.fb.call(cur, self.fits.clone(), args, Some(r));
        let (yes, no) = (self.fb.block(), self.fb.block());
        self.fb.branch(after, r, yes, no);
        let n = self.number;
        self.number += 1;
        self.result(no, int(n.into(), Ty::U64));
        yes
    }

    /// `fits(a, at + i + k)`.
    fn test_at(&mut self, cur: BlockId, k: u64) -> BlockId {
        let x = self.add(cur, copy_local(self.at), copy_local(self.i));
        let y = self.add(cur, copy_local(x), int(k.into(), Ty::U64));
        self.test(cur, y)
    }

    /// `i = i + by` through a temp, in `b`.
    fn increment(&mut self, b: BlockId, by: u64) {
        let next = self.add(b, copy_local(self.i), int(by.into(), Ty::U64));
        self.fb.assign(b, self.i, Rvalue::Use(copy_local(next)));
    }

    /// `count += 1` and back to the head, from `b`.
    fn latch(&mut self, b: BlockId) {
        self.fb.assign(
            b,
            self.count,
            bin(BinOp::Add, copy_local(self.count), int(1, Ty::U64)),
        );
        self.fb.goto(b, self.head);
    }

    fn finish(mut self) -> Program {
        self.fb.goto(self.entry, self.head);
        self.pb.add(self.fb.finish());
        self.pb.finish()
    }
}

/// `(la, at, dim, i0, x0, flag)` at the edges: lengths and bounds around the step, offsets that
/// wrap, a stale value `x0` inside, at and past the end, and both flags.
fn grid() -> Vec<[u64; 6]> {
    let top = u64::MAX;
    let mut g = vec![];
    for la in [0, 4, 15, 16, 20, 24, 28, 31, 32, 36, 40, 48, 64] {
        for at in [0, 1, 4, 8, 12, 16, top - 15, top] {
            for dim in [0, 15, 16, 24, 32, 33, 48] {
                for x0 in [0, 28, la, top - 2] {
                    for flag in [0, 1] {
                        g.push([la, at, dim, 0, x0, flag]);
                    }
                }
            }
        }
    }
    g
}

/// Whether the pass copies the loop; the copy agrees with the original on the grid.
fn copied(p: &Program) -> bool {
    let (q, changed) = versioned(p);
    agree(p, &q, &grid());
    changed
}

/// The calls left in the copy that the pass made: all of `f`'s minus the original's.
fn calls_added(p: &Program) -> isize {
    let (q, _) = versioned(p);
    count_calls(&q.funcs[0]) as isize - count_calls(&p.funcs[0]) as isize
}

#[test]
fn a_counter_assigned_twice_one_after_the_other_decides_nothing() {
    // fits(a, at + i); i += 8; fits(a, at + i); i += 8: the second test sees the next `i`.
    let mut k = Kit::new();
    k.close_head();
    let b = k.test_at(k.body, 0);
    k.increment(b, STEP / 2);
    let b = k.test_at(b, 0);
    k.increment(b, STEP / 2);
    k.latch(b);
    let p = k.finish();
    assert!(!copied(&p));
}

#[test]
fn a_counter_assigned_on_each_branch_decides_nothing() {
    // fits(a, at + i); if (flag) { i += 16; fits(a, at + i) } else { i += 16 }: the test after
    // the first assignment sees the next `i`. The branch without a test comes last, so a pass
    // that took the last assignment for the counter's would call that test guarded.
    let mut k = Kit::new();
    k.close_head();
    let b = k.test_at(k.body, 0);
    let on = k.local(Ty::Bool);
    k.fb.assign(b, on, bin(BinOp::Ne, copy_local(k.flag), int(0, Ty::U64)));
    let then = k.fb.block();
    k.increment(then, STEP);
    let then_end = k.test_at(then, 0);
    k.latch(then_end);
    let els = k.fb.block();
    k.increment(els, STEP);
    k.latch(els);
    k.fb.branch(b, on, then, els);
    let p = k.finish();
    assert!(!copied(&p));
}

#[test]
fn an_index_assigned_on_one_branch_only_keeps_its_test() {
    // x = x0 in front; fits(a, at + i); if (flag) { x = at + i }; fits(a, x): where `flag` is
    // false, `x` is `x0` or an earlier iteration's index, not this one's.
    let mut k = Kit::new();
    let x = k.local(Ty::U64);
    k.fb.assign(k.entry, x, Rvalue::Use(copy_local(k.x0)));
    k.close_head();
    let b = k.test_at(k.body, 0);
    let on = k.local(Ty::Bool);
    k.fb.assign(b, on, bin(BinOp::Ne, copy_local(k.flag), int(0, Ty::U64)));
    let (set, join) = (k.fb.block(), k.fb.block());
    k.fb.assign(set, x, bin(BinOp::Add, copy_local(k.at), copy_local(k.i)));
    k.fb.goto(set, join);
    k.fb.branch(b, on, set, join);
    let end = k.test(join, x);
    k.increment(end, STEP);
    k.latch(end);
    let p = k.finish();
    // The first test is decided; the second stays in the copy.
    assert!(copied(&p));
    assert_eq!(calls_added(&p), 1);
}

#[test]
fn a_comparison_in_the_head_before_the_counters_test_is_kept() {
    // The head computes `ok = at + i < la` before `i + 16 <= dim`, and the exit returns it: on
    // the last pass through the head, `i` is past the bound.
    let mut k = Kit::new();
    let x = k.add(k.head, copy_local(k.at), copy_local(k.i));
    let ok = k.local(Ty::Bool);
    k.fb.assign(k.head, ok, bin(BinOp::Lt, copy_local(x), copy_local(k.la)));
    k.close_head();
    let b = k.test_at(k.body, 0);
    k.increment(b, STEP);
    k.latch(b);
    let flag = k.local(Ty::U64);
    k.fb.assign(k.exit, flag, Rvalue::Cast(copy_local(ok), Ty::U64));
    let extra = k.add(k.exit, copy_local(flag), int(1000, Ty::U64));
    k.result(k.exit, copy_local(extra));
    let p = k.finish();
    assert!(copied(&p));
}

#[test]
fn a_loop_that_copies_memory_into_a_header_is_not_copied() {
    // Each iteration copies a header of length `x0` over `a`'s: the length changes.
    let mut k = Kit::new();
    let other = k.local(Ty::Agg(k.agg));
    k.fb.push(
        k.entry,
        Stmt::Assign(field(other, 1), Rvalue::Use(copy_local(k.x0))),
    );
    let src = k.local(Ty::Ptr);
    k.fb.assign(k.entry, src, Rvalue::AddrOf(Place::local(other)));
    k.close_head();
    let b = k.test_at(k.body, 0);
    k.fb.push(
        b,
        Stmt::MemCopy {
            dst: copy_local(k.pa),
            src: copy_local(src),
            size: 24,
        },
    );
    let b = k.test_at(b, 4);
    k.increment(b, STEP);
    k.latch(b);
    let p = k.finish();
    assert!(!copied(&p));
}

#[test]
fn a_bound_whose_address_is_taken_decides_nothing() {
    // `&dim` is taken in front of the loop. The loop writes no memory, so it can't change, but the
    // pass reasons only about values that live in registers.
    let mut k = Kit::new();
    let addr = k.local(Ty::Ptr);
    k.fb.assign(k.entry, addr, Rvalue::AddrOf(Place::local(k.dim)));
    k.close_head();
    let b = k.test_at(k.body, 0);
    let b = k.test_at(b, 12);
    k.increment(b, STEP);
    k.latch(b);
    let p = k.finish();
    assert!(!copied(&p));
}
