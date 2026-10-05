//! Property test for `heap_sroa`: random programs moving a few objects between pointer locals
//! (copies, fresh objects, field writes and reads, in loops and diamonds) must compute the same
//! result after the pass, and after the whole speed pipeline, as with every object on the heap.
//! This checks the value-semantics analysis (`flow`) against the interpreter's references.

use super::*;
use crate::interp::{Interp, Memory, Trap};
use crate::testkit::builder::*;
use crate::testkit::validate::assert_valid;
use velt_vir::vir::Ty::*;
use velt_vir::vir::{AggId, BinOp, BlockId, Callee, ExternFn, Local, Place, Proj, Rvalue, Stmt};

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

/// A heap that never frees (the programs leak their objects) and records `keep` calls.
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
    keep: Option<ExternId>,
    obj: AggId,
    ptrs: Vec<Local>,
    acc: Local,
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

    /// `w = new Obj(x, y)` as lowering emits it; returns the continuation.
    fn new_obj(&mut self, b: BlockId, w: Local) -> BlockId {
        let (x, y) = (self.value(b), self.value(b));
        let args = vec![int(16, U64), int(8, U64)];
        let b = self.fb.call(b, Callee::Extern(self.alloc), args, Some(w));
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

    /// One random operation at the end of `b`; returns the continuation.
    fn op(&mut self, b: BlockId) -> BlockId {
        let w = self.any_ptr();
        match self.rng.below(10) {
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
            _ => match self.keep {
                Some(keep) => {
                    let v = self.value(b);
                    let args = vec![copy_local(w), copy_local(v)];
                    self.fb.call(b, Callee::Extern(keep), args, None)
                }
                None => b,
            },
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
fn random_program(seed: u64, escapes: bool) -> Program {
    let mut pb = ProgramBuilder::new();
    let alloc = pb.ext("velt_rt_alloc", &[U64, U64], Ptr, false);
    pb.ext("velt_rt_free", &[Ptr, U64, U64], Unit, false);
    let keep = pb.ext("keep", &[Ptr, I64], Unit, false);
    let obj = pb.agg("Pair object", 16, 8, &[(I64, 0), (I64, 8)]);
    let mut fb = FuncBuilder::export("main", &[I64], I64);
    let ptrs = (0..PTRS).map(|_| fb.local(Ptr)).collect();
    let acc = fb.local(I64);
    let mut g = Gen {
        rng: Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1),
        fb,
        alloc,
        keep: escapes.then_some(keep),
        obj,
        ptrs,
        acc,
    };
    let n = g.fb.param(0);
    let (i, c) = (g.fb.local(I64), g.fb.local(Bool));
    let mut b = g.fb.block();
    g.fb.assign(b, acc, Rvalue::Use(int(1, I64)));
    g.fb.assign(b, i, Rvalue::Use(int(0, I64)));
    for k in 0..PTRS {
        let w = g.ptrs[k];
        b = g.fb.call(
            b,
            Callee::Extern(alloc),
            vec![int(16, U64), int(8, U64)],
            Some(w),
        );
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
    }
    g.fb.ret(exit, copy_local(acc));
    pb.add(g.fb.finish());
    pb.finish()
}

#[test]
fn random_programs_keep_their_results() {
    let mut replaced = 0;
    for seed in 0..1500u64 {
        let original = random_program(seed, seed % 4 == 0);
        let mut alone = original.clone();
        let aggs = alone.aggs.clone();
        let allocator = Allocator::find(&alone);
        replaced += usize::from(run(&aggs, allocator, &mut alone.funcs[0]));
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
    // The generator must exercise the rewrite, not only the cases that keep the heap.
    assert!(
        replaced > 300,
        "only {replaced} programs had objects replaced"
    );
}
