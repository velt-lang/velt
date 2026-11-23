//! Property test for `heap_sroa`: random programs moving a few objects between pointer locals
//! (copies, fresh objects, field writes and reads, drops with their null tests, in loops and
//! diamonds) must compute the same
//! result after the pass, and after the whole speed pipeline, as with every object on the heap.
//! This checks the value-semantics analysis (`flow`) against the interpreter's references.
//!
//! The counted variant (#1718) makes every object a counted block, `[u64 count][object]`, with
//! the object pointer 8 bytes in: retains and releases through `o - 8` (a release tests the
//! count for 1 and frees the block), header names `h = o - 8` and `o = h + 8` copied in loops and
//! diamonds, and the counts folded into the result, so the count and the object, under two
//! names, must stay together.

use super::*;
use crate::interp::{Interp, Memory, Trap};
use crate::testkit::builder::*;
use crate::testkit::validate::assert_valid;
use velt_vir::vir::Ty::*;
use velt_vir::vir::{
    AggId, BinOp, BlockId, Callee, ExternFn, Local, Operand, Place, Proj, Rvalue, Stmt,
};

/// Deterministic xorshift generator.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// A heap that never reuses memory (a program may read an object it freed while another local
/// still holds it; both runs then read the same bytes) and records `keep` calls.
#[derive(Default)]
struct Host {
    allocs: u64,
    kept: Vec<u64>,
}

impl crate::interp::Host for Host {
    fn call(&mut self, ext: &ExternFn, args: &[u64], mem: &mut Memory) -> Result<u64, Trap> {
        match ext.symbol.as_str() {
            "velt_rt_alloc" => {
                self.allocs += 1;
                mem.alloc_heap(args[0], args[1])
            }
            "velt_rt_free" => Ok(0),
            _ => {
                self.kept.push(args[1]);
                Ok(0)
            }
        }
    }
}

/// (result, objects allocated, `keep` arguments)
fn execute(p: &Program, n: u64) -> (u64, u64, Vec<u64>) {
    let mut interp = Interp::new(p, Host::default());
    let r = interp.call_symbol("main", &[n]).expect("program runs");
    (r, interp.host.allocs, interp.host.kept)
}

/// Pointer locals of a random program.
const PTRS: usize = 4;

struct Gen {
    rng: Rng,
    fb: FuncBuilder,
    alloc: ExternId,
    free: ExternId,
    keep: Option<ExternId>,
    obj: AggId,
    ptrs: Vec<Local>,
    /// Header names (`h = o - 8`) of the counted variant.
    hdrs: Vec<Local>,
    acc: Local,
    /// Objects are counted blocks.
    counted: bool,
}

impl Gen {
    fn field(&self, w: Local, k: u32) -> Place {
        Place {
            local: w,
            proj: vec![Proj::Deref(Ty::Agg(self.obj)), Proj::Field(k)],
        }
    }

    fn any_ptr(&mut self) -> Local {
        self.ptrs[self.rng.below(PTRS)]
    }

    fn any_field(&mut self) -> Place {
        let w = self.any_ptr();
        let k = self.rng.below(2) as u32;
        self.field(w, k)
    }

    /// A scalar computed from a field of some object.
    fn value(&mut self, b: BlockId) -> Local {
        let t = self.fb.local(I64);
        let c = self.rng.below(7) as i128 + 1;
        let f = self.any_field();
        self.fb
            .assign(b, t, bin(BinOp::Add, copy_place(f), int(c, I64)));
        t
    }

    /// The block size: the object's, and the count in front of it when counted.
    fn size(&self) -> i128 {
        if self.counted {
            24
        } else {
            16
        }
    }

    /// Allocate `w`'s block: `w` itself, or (counted) a block with a count of 1 and `w` 8 bytes
    /// in; returns the continuation.
    fn alloc_obj(&mut self, b: BlockId, w: Local) -> BlockId {
        let args = vec![int(self.size(), U64), int(8, U64)];
        if !self.counted {
            return self.fb.call(b, Callee::Extern(self.alloc), args, Some(w));
        }
        let base = self.fb.local(Ptr);
        let b = self
            .fb
            .call(b, Callee::Extern(self.alloc), args, Some(base));
        let count = Stmt::Assign(deref(base, U64), Rvalue::Use(int(1, U64)));
        self.fb.push(b, count);
        self.fb
            .assign(b, w, bin(BinOp::PtrAdd, copy_local(base), int(8, I64)));
        b
    }

    /// `h = w - 8`, the count's address, in a new local.
    fn header(&mut self, b: BlockId, w: Local) -> Local {
        let h = self.fb.local(Ptr);
        self.fb
            .assign(b, h, bin(BinOp::PtrAdd, copy_local(w), int(-8, I64)));
        h
    }

    /// `acc = acc * 31 + v`
    fn fold(&mut self, b: BlockId, v: Operand) {
        let (t, acc) = (self.fb.local(I64), self.acc);
        self.fb
            .assign(b, t, bin(BinOp::Mul, copy_local(acc), int(31, I64)));
        self.fb.assign(b, acc, bin(BinOp::Add, copy_local(t), v));
    }

    /// Fold the count at `h` into `acc`.
    fn fold_count(&mut self, b: BlockId, h: Local) {
        let (c, v) = (self.fb.local(U64), self.fb.local(I64));
        self.fb.assign(b, c, Rvalue::Use(copy_place(deref(h, U64))));
        self.fb.assign(b, v, Rvalue::Cast(copy_local(c), I64));
        self.fold(b, copy_local(v));
    }

    /// A retain of `w`: its count plus 1.
    fn retain(&mut self, b: BlockId, w: Local) {
        let (h, c) = (self.header(b, w), self.fb.local(U64));
        self.fb.assign(b, c, Rvalue::Use(copy_place(deref(h, U64))));
        let inc = bin(BinOp::Add, copy_local(c), int(1, U64));
        self.fb.push(b, Stmt::Assign(deref(h, U64), inc));
    }

    /// A release of `w` as lowering emits it: `if (count == 1) free(w - 8) else count -= 1`;
    /// returns the continuation.
    fn release(&mut self, b: BlockId, w: Local) -> BlockId {
        let h = self.header(b, w);
        let (c, one) = (self.fb.local(U64), self.fb.local(Bool));
        self.fb.assign(b, c, Rvalue::Use(copy_place(deref(h, U64))));
        self.fb
            .assign(b, one, bin(BinOp::Eq, copy_local(c), int(1, U64)));
        let (last, shared, next) = (self.fb.block(), self.fb.block(), self.fb.block());
        self.fb.branch(b, one, last, shared);
        let args = vec![copy_local(h), int(24, U64), int(8, U64)];
        let after = self.fb.call(last, Callee::Extern(self.free), args, None);
        self.fb.goto(after, next);
        let dec = bin(BinOp::Sub, copy_local(c), int(1, U64));
        self.fb.push(shared, Stmt::Assign(deref(h, U64), dec));
        self.fb.goto(shared, next);
        next
    }

    /// One of the header names' operations: `h = w - 8`, `w = h + 8`, or the count at `h`
    /// folded into the result.
    fn header_op(&mut self, b: BlockId, w: Local) {
        let h = self.hdrs[self.rng.below(PTRS)];
        match self.rng.below(3) {
            0 => self
                .fb
                .assign(b, h, bin(BinOp::PtrAdd, copy_local(w), int(-8, I64))),
            1 => self
                .fb
                .assign(b, w, bin(BinOp::PtrAdd, copy_local(h), int(8, I64))),
            _ => self.fold_count(b, h),
        }
    }

    /// `w = new Obj(x, y)` as lowering emits it; returns the continuation.
    fn new_obj(&mut self, b: BlockId, w: Local) -> BlockId {
        let (x, y) = (self.value(b), self.value(b));
        let b = self.alloc_obj(b, w);
        let fill = Stmt::MemSet {
            dst: copy_local(w),
            byte: int(0, U8),
            len: int(16, U64),
        };
        self.fb.push(b, fill);
        let (f0, f1) = (self.field(w, 0), self.field(w, 1));
        self.fb
            .push(b, Stmt::Assign(f0, Rvalue::Use(copy_local(x))));
        self.fb
            .push(b, Stmt::Assign(f1, Rvalue::Use(copy_local(y))));
        b
    }

    /// The drop of `w` as lowering emits it: `t = w; if (w != null) free(t)` (counted: a
    /// release); returns the continuation.
    fn drop_obj(&mut self, b: BlockId, w: Local) -> BlockId {
        if self.counted {
            return self.release(b, w);
        }
        let (t, c) = (self.fb.local(Ptr), self.fb.local(Bool));
        self.fb.assign(b, t, Rvalue::Use(copy_local(w)));
        self.fb
            .assign(b, c, bin(BinOp::Ne, copy_local(w), int(0, Ptr)));
        let (free, next) = (self.fb.block(), self.fb.block());
        self.fb.branch(b, c, free, next);
        let args = vec![copy_local(t), int(16, U64), int(8, U64)];
        let after = self.fb.call(free, Callee::Extern(self.free), args, None);
        self.fb.goto(after, next);
        next
    }

    /// One random operation at the end of `b`; returns the continuation.
    fn op(&mut self, b: BlockId) -> BlockId {
        let w = self.any_ptr();
        // The plain variant draws as it did before the counted operations (12 and up) existed,
        // so its programs are the same.
        match self.rng.below(if self.counted { 16 } else { 12 }) {
            0 | 1 => self.new_obj(b, w),
            2..=4 => {
                let src = self.any_ptr();
                self.fb.assign(b, w, Rvalue::Use(copy_local(src)));
                b
            }
            5 | 6 => {
                let v = self.value(b);
                let f = self.any_field();
                self.fb.push(b, Stmt::Assign(f, Rvalue::Use(copy_local(v))));
                b
            }
            7 | 8 => {
                let v = self.value(b);
                let t = self.fb.local(I64);
                let acc = self.acc;
                self.fb
                    .assign(b, t, bin(BinOp::Mul, copy_local(acc), int(31, I64)));
                self.fb
                    .assign(b, acc, bin(BinOp::Add, copy_local(t), copy_local(v)));
                b
            }
            9 => {
                let b = self.drop_obj(b, w);
                self.new_obj(b, w)
            }
            10 => {
                let (nonnull, bit) = (self.fb.local(Bool), self.fb.local(I64));
                let acc = self.acc;
                self.fb
                    .assign(b, nonnull, bin(BinOp::Ne, copy_local(w), int(0, Ptr)));
                self.fb
                    .assign(b, bit, Rvalue::Cast(copy_local(nonnull), I64));
                self.fb
                    .assign(b, acc, bin(BinOp::Add, copy_local(acc), copy_local(bit)));
                b
            }
            11 => match self.keep {
                Some(keep) => {
                    let v = self.value(b);
                    let args = vec![copy_local(w), copy_local(v)];
                    self.fb.call(b, Callee::Extern(keep), args, None)
                }
                None => b,
            },
            12 => {
                self.retain(b, w);
                b
            }
            13 => self.release(b, w),
            14 => {
                let h = self.header(b, w);
                self.fold_count(b, h);
                b
            }
            _ => {
                self.header_op(b, w);
                b
            }
        }
    }

    /// A few operations, or a diamond on the low bit of `acc` with operations on both sides.
    fn segment(&mut self, b: BlockId) -> BlockId {
        if self.rng.below(3) > 0 {
            return (0..1 + self.rng.below(4)).fold(b, |b, _| self.op(b));
        }
        let (bit, cond) = (self.fb.local(I64), self.fb.local(Bool));
        let acc = self.acc;
        self.fb
            .assign(b, bit, bin(BinOp::BitAnd, copy_local(acc), int(1, I64)));
        self.fb
            .assign(b, cond, bin(BinOp::Ne, copy_local(bit), int(0, I64)));
        let (then, els, join) = (self.fb.block(), self.fb.block(), self.fb.block());
        self.fb.branch(b, cond, then, els);
        for side in [then, els] {
            let end = (0..self.rng.below(3)).fold(side, |b, _| self.op(b));
            self.fb.goto(end, join);
        }
        join
    }
}

/// `main(n)`: every pointer starts with a fresh object, then `n` iterations of random segments,
/// then the sum of every object's fields is folded into the result.
fn random_program(seed: u64, escapes: bool, counted: bool) -> Program {
    let mut pb = ProgramBuilder::new();
    let alloc = pb.ext("velt_rt_alloc", &[U64, U64], Ptr, false);
    let free = pb.ext("velt_rt_free", &[Ptr, U64, U64], Unit, false);
    let keep = pb.ext("keep", &[Ptr, I64], Unit, false);
    let obj = pb.agg("Pair object", 16, 8, &[(I64, 0), (I64, 8)]);
    let mut fb = FuncBuilder::export("main", &[I64], I64);
    let ptrs = (0..PTRS).map(|_| fb.local(Ptr)).collect();
    let hdrs = (0..PTRS).map(|_| fb.local(Ptr)).collect();
    let acc = fb.local(I64);
    let mut g = Gen {
        rng: Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1),
        fb,
        alloc,
        free,
        keep: escapes.then_some(keep),
        obj,
        ptrs,
        hdrs,
        acc,
        counted,
    };
    let n = g.fb.param(0);
    let (i, c) = (g.fb.local(I64), g.fb.local(Bool));
    let mut b = g.fb.block();
    g.fb.assign(b, acc, Rvalue::Use(int(1, I64)));
    g.fb.assign(b, i, Rvalue::Use(int(0, I64)));
    for k in 0..PTRS {
        let w = g.ptrs[k];
        b = g.alloc_obj(b, w);
        let h = g.hdrs[k];
        g.fb.assign(b, h, bin(BinOp::PtrAdd, copy_local(w), int(-8, I64)));
        let (f0, f1) = (g.field(w, 0), g.field(w, 1));
        g.fb.push(b, Stmt::Assign(f0, Rvalue::Use(int(k as i128, I64))));
        g.fb.push(b, Stmt::Assign(f1, Rvalue::Use(int(10 * k as i128, I64))));
    }
    let (head, body, exit) = (g.fb.block(), g.fb.block(), g.fb.block());
    g.fb.goto(b, head);
    g.fb.assign(head, c, bin(BinOp::Lt, copy_local(i), copy_local(n)));
    g.fb.branch(head, c, body, exit);
    let end = (0..1 + g.rng.below(4)).fold(body, |b, _| g.segment(b));
    g.fb.assign(end, i, bin(BinOp::Add, copy_local(i), int(1, I64)));
    g.fb.goto(end, head);
    for k in 0..PTRS {
        let w = g.ptrs[k];
        for f in 0..2 {
            let t = g.fb.local(I64);
            let place = g.field(w, f);
            g.fb.assign(exit, t, bin(BinOp::Mul, copy_local(acc), int(7, I64)));
            g.fb.assign(exit, acc, bin(BinOp::Add, copy_local(t), copy_place(place)));
        }
        if counted {
            let h = g.header(exit, w);
            g.fold_count(exit, h);
        }
    }
    g.fb.ret(exit, copy_local(acc));
    pb.add(g.fb.finish());
    pb.finish()
}

/// Run `random_program` for 1500 seeds, alone and through the speed pipeline; returns how many
/// had objects replaced.
fn check_random_programs(counted: bool) -> usize {
    let mut replaced = 0;
    for seed in 0..1500u64 {
        let original = random_program(seed, seed % 4 == 0, counted);
        let mut alone = original.clone();
        let aggs = alone.aggs.clone();
        let allocator = Allocator::find(&alone);
        replaced += usize::from(run(&aggs, allocator, &mut alone.funcs[0]).replaced);
        assert_valid(&alone);
        let mut pipeline = original.clone();
        crate::optimize(&mut pipeline, crate::OptLevel::Speed);
        assert_valid(&pipeline);
        for n in [0, 1, 2, 5] {
            let (want, _, kept) = execute(&original, n);
            for p in [&alone, &pipeline] {
                let (got, _, got_kept) = execute(p, n);
                assert_eq!((want, &kept), (got, &got_kept), "seed {seed}, n = {n}\n{p}");
            }
        }
    }
    replaced
}

#[test]
fn random_programs_keep_their_results() {
    let replaced = check_random_programs(false);
    // The generator must exercise the rewrite, not only the cases that keep the heap.
    assert!(
        replaced > 300,
        "only {replaced} programs had objects replaced"
    );
}

#[test]
fn random_counted_programs_keep_their_results() {
    let replaced = check_random_programs(true);
    assert!(
        replaced > COUNTED_REPLACED,
        "only {replaced} counted programs had objects replaced"
    );
}

/// How many of the 1500 counted programs must have objects replaced (1339 are).
const COUNTED_REPLACED: usize = 1000;
