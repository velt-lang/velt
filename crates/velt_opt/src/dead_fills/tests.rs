//! Unit tests for dropping dead zero fills on hand-built VIR: each program runs in the
//! interpreter before and after the pass, with a heap whose new blocks hold garbage, so a fill
//! dropped too early changes the result.

use super::*;
use crate::interp::{Interp, Memory, Trap};
use crate::testkit::builder::*;
use crate::testkit::validate::assert_valid;
use velt_vir::vir::Ty::*;
use velt_vir::vir::{
    AggId, BinOp, BlockId, Callee, ExternFn, ExternId, Local, Operand, Place, Program, Proj,
    Rvalue, Stmt, Ty,
};

/// A heap whose new blocks are filled with `0xA5`; other extern calls return 0.
#[derive(Default)]
struct GarbageHeap;

impl crate::interp::Host for GarbageHeap {
    fn call(&mut self, ext: &ExternFn, args: &[u64], mem: &mut Memory) -> Result<u64, Trap> {
        match ext.symbol.as_str() {
            "velt_rt_alloc" => {
                let p = mem.alloc_heap(args[0], args[1])?;
                mem.write(p, &vec![0xA5; args[0] as usize])?;
                Ok(p)
            }
            _ => Ok(0),
        }
    }
}

fn run_main(p: &Program, n: u64) -> u64 {
    let mut interp = Interp::new(p, GarbageHeap);
    interp.call_symbol("main", &[n]).expect("program runs")
}

/// Builder state: the allocator, an opaque `keep(ptr)` and `tick()`, and the object layouts.
struct Env {
    pb: ProgramBuilder,
    alloc: ExternId,
    keep: ExternId,
    tick: ExternId,
    /// `{ i64, i64 }`
    pair: AggId,
    /// `{ bool, i64 }`: 7 bytes of padding.
    padded: AggId,
}

fn env() -> Env {
    let mut pb = ProgramBuilder::new();
    let alloc = pb.ext("velt_rt_alloc", &[U64, U64], Ptr, false);
    pb.ext("velt_rt_free", &[Ptr, U64, U64], Unit, false);
    let keep = pb.ext("keep", &[Ptr], Unit, false);
    let tick = pb.ext("tick", &[], Unit, false);
    let pair = pb.agg("Pair object", 16, 8, &[(I64, 0), (I64, 8)]);
    let padded = pb.agg("Padded object", 16, 8, &[(Bool, 0), (I64, 8)]);
    Env {
        pb,
        alloc,
        keep,
        tick,
        pair,
        padded,
    }
}

fn field(w: Local, obj: AggId, k: u32) -> Place {
    Place {
        local: w,
        proj: vec![Proj::Deref(Ty::Agg(obj)), Proj::Field(k)],
    }
}

fn store(fb: &mut FuncBuilder, b: BlockId, p: Place, v: Operand) {
    fb.push(b, Stmt::Assign(p, Rvalue::Use(v)));
}

fn fill(fb: &mut FuncBuilder, b: BlockId, w: Local, len: i128) {
    let fill = Stmt::MemSet {
        dst: copy_local(w),
        byte: int(0, U8),
        len: int(len, U64),
    };
    fb.push(b, fill);
}

impl Env {
    /// `w = alloc(size); memset w, 0, size`; returns the block after the fill.
    fn alloc(&self, fb: &mut FuncBuilder, b: BlockId, w: Local, size: i128) -> BlockId {
        let args = vec![int(size, U64), int(8, U64)];
        let b = fb.call(b, Callee::Extern(self.alloc), args, Some(w));
        fill(fb, b, w, size);
        b
    }

    /// `keep(w); return w.0 + w.1` (as `pair`) or `w.1 + w.0 as i64` (as `padded`).
    fn finish(mut self, mut fb: FuncBuilder, b: BlockId, w: Local, obj: AggId) -> Program {
        let b = fb.call(b, Callee::Extern(self.keep), vec![copy_local(w)], None);
        let (x, r) = (fb.local(I64), fb.local(I64));
        let first = copy_place(field(w, obj, 0));
        let ty = if obj == self.pair { I64 } else { Bool };
        let first = if ty == I64 {
            Rvalue::Use(first)
        } else {
            Rvalue::Cast(first, I64)
        };
        fb.assign(b, x, first);
        let second = copy_place(field(w, obj, 1));
        fb.assign(b, r, bin(BinOp::Add, copy_local(x), second));
        fb.ret(b, copy_local(r));
        self.pb.add(fb.finish());
        self.pb.finish()
    }
}

/// Run the pass; check validity, the result for a few inputs, and whether it dropped a fill.
fn check(original: Program, expect_dropped: bool) {
    let mut p = original.clone();
    let aggs = p.aggs.clone();
    let allocator = Allocator::find(&p);
    let changed = run(&aggs, allocator, &mut p.funcs[0]);
    assert_eq!(changed, expect_dropped, "{p}");
    assert_valid(&p);
    for n in [0, 1, 9] {
        assert_eq!(run_main(&original, n), run_main(&p, n), "n = {n}\n{p}");
    }
    let fills = p.funcs[0].blocks.iter().flat_map(|b| &b.stmts);
    let left = fills.filter(|s| matches!(s, Stmt::MemSet { .. })).count();
    assert_eq!(left, usize::from(!expect_dropped), "{p}");
}

/// `main(n)`: a pair object, then `body` between the fill and `keep`.
fn pair_program(body: impl FnOnce(&Env, &mut FuncBuilder, BlockId, Local) -> BlockId) -> Program {
    let env = env();
    let mut fb = FuncBuilder::export("main", &[I64], I64);
    let w = fb.local(Ptr);
    let b = fb.block();
    let b = env.alloc(&mut fb, b, w, 16);
    let b = body(&env, &mut fb, b, w);
    let pair = env.pair;
    env.finish(fb, b, w, pair)
}

#[test]
fn drops_the_fill_of_an_object_whose_fields_are_all_stored() {
    let p = pair_program(|env, fb, b, w| {
        let n = copy_local(fb.param(0));
        store(fb, b, field(w, env.pair, 0), n);
        store(fb, b, field(w, env.pair, 1), int(2, I64));
        b
    });
    check(p, true);
}

#[test]
fn keeps_the_fill_when_a_field_is_not_stored() {
    let p = pair_program(|env, fb, b, w| {
        store(fb, b, field(w, env.pair, 1), int(2, I64));
        b
    });
    check(p, false);
}

#[test]
fn a_read_of_a_stored_field_is_fine_an_unstored_one_is_not() {
    for read_stored in [true, false] {
        let p = pair_program(|env, fb, b, w| {
            let x = fb.local(I64);
            store(fb, b, field(w, env.pair, 0), int(5, I64));
            let k = u32::from(!read_stored);
            let read = bin(BinOp::Add, copy_place(field(w, env.pair, k)), int(1, I64));
            fb.assign(b, x, read);
            store(fb, b, field(w, env.pair, 1), copy_local(x));
            b
        });
        check(p, read_stored);
    }
}

#[test]
fn keeps_the_fill_when_the_pointer_escapes_first() {
    // Passed to a call, or its field's address taken, before the second store.
    for by_call in [true, false] {
        let p = pair_program(|env, fb, b, w| {
            store(fb, b, field(w, env.pair, 0), int(5, I64));
            let b = if by_call {
                fb.call(b, Callee::Extern(env.keep), vec![copy_local(w)], None)
            } else {
                let a = fb.local(Ptr);
                fb.assign(b, a, Rvalue::AddrOf(field(w, env.pair, 1)));
                b
            };
            store(fb, b, field(w, env.pair, 1), int(2, I64));
            b
        });
        check(p, false);
    }
}

#[test]
fn keeps_the_fill_when_a_branch_comes_first() {
    let p = pair_program(|env, fb, b, w| {
        store(fb, b, field(w, env.pair, 0), int(5, I64));
        let c = fb.local(Bool);
        let n = copy_local(fb.param(0));
        fb.assign(b, c, bin(BinOp::Lt, n, int(3, I64)));
        let (then, join) = (fb.block(), fb.block());
        fb.branch(b, c, then, join);
        fb.goto(then, join);
        store(fb, join, field(w, env.pair, 1), int(2, I64));
        join
    });
    check(p, false);
}

#[test]
fn follows_gotos_copies_and_calls_that_do_not_see_the_object() {
    let p = pair_program(|env, fb, b, w| {
        let q = fb.local(Ptr);
        fb.assign(b, q, Rvalue::Use(copy_local(w)));
        store(fb, b, field(q, env.pair, 0), int(5, I64));
        let b = fb.call(b, Callee::Extern(env.tick), vec![], None);
        let next = fb.block();
        fb.goto(b, next);
        store(fb, next, field(w, env.pair, 1), int(2, I64));
        next
    });
    check(p, true);
}

#[test]
fn stores_before_the_fill_do_not_count() {
    let env = env();
    let mut fb = FuncBuilder::export("main", &[I64], I64);
    let w = fb.local(Ptr);
    let b = fb.block();
    let args = vec![int(16, U64), int(8, U64)];
    let b = fb.call(b, Callee::Extern(env.alloc), args, Some(w));
    store(&mut fb, b, field(w, env.pair, 0), int(5, I64));
    fill(&mut fb, b, w, 16);
    store(&mut fb, b, field(w, env.pair, 1), int(2, I64));
    let pair = env.pair;
    check(env.finish(fb, b, w, pair), false);
}

#[test]
fn a_counted_object_behind_its_header() {
    // `a = alloc(24); *a = 1; p = a + 8; memset p, 0, 16; p.0 = n; p.1 = 2`
    let env = env();
    let mut fb = FuncBuilder::export("main", &[I64], I64);
    let (a, p) = (fb.local(Ptr), fb.local(Ptr));
    let b = fb.block();
    let args = vec![int(24, U64), int(8, U64)];
    let b = fb.call(b, Callee::Extern(env.alloc), args, Some(a));
    store(&mut fb, b, deref(a, U64), int(1, U64));
    fb.assign(b, p, bin(BinOp::PtrAdd, copy_local(a), int(8, I64)));
    fill(&mut fb, b, p, 16);
    let n = copy_local(fb.param(0));
    store(&mut fb, b, field(p, env.pair, 0), n);
    store(&mut fb, b, field(p, env.pair, 1), int(2, I64));
    let pair = env.pair;
    check(env.finish(fb, b, p, pair), true);
}

#[test]
fn padding_needs_no_zeros_unless_seen_as_bytes() {
    // `{ bool, i64 }` stored field by field: the padding after the bool stays unwritten. With
    // the `i64` stored as `*(w + 8)`, the stores do not share one view, so the padding counts.
    for as_bytes in [false, true] {
        let env = env();
        let mut fb = FuncBuilder::export("main", &[I64], I64);
        let (w, q) = (fb.local(Ptr), fb.local(Ptr));
        let b = fb.block();
        let b = env.alloc(&mut fb, b, w, 16);
        store(&mut fb, b, field(w, env.padded, 0), boolean(true));
        if as_bytes {
            fb.assign(b, q, bin(BinOp::PtrAdd, copy_local(w), int(8, I64)));
            store(&mut fb, b, deref(q, I64), int(7, I64));
        } else {
            store(&mut fb, b, field(w, env.padded, 1), int(7, I64));
        }
        let padded = env.padded;
        check(env.finish(fb, b, w, padded), !as_bytes);
    }
}

#[test]
fn a_fill_in_a_block_another_path_reaches_is_kept() {
    // `k = new(7, 8); w = alloc; q = n < 3 ? k : w;` then, where both paths meet:
    // `memset w; w.0 = 1; x = q.1; w.1 = 2; return x`. From the left allocation the stores
    // cover the fill, but on the right path `q` is `w`, and `x` must read the zero.
    let mut env = env();
    let mut fb = FuncBuilder::export("main", &[I64], I64);
    let (k, w, q, c, x) = (
        fb.local(Ptr),
        fb.local(Ptr),
        fb.local(Ptr),
        fb.local(Bool),
        fb.local(I64),
    );
    let b = fb.block();
    let b = env.alloc(&mut fb, b, k, 16);
    store(&mut fb, b, field(k, env.pair, 0), int(7, I64));
    store(&mut fb, b, field(k, env.pair, 1), int(8, I64));
    let n = copy_local(fb.param(0));
    fb.assign(b, c, bin(BinOp::Lt, n, int(3, I64)));
    let (left, right, join) = (fb.block(), fb.block(), fb.block());
    fb.branch(b, c, left, right);
    let args = vec![int(16, U64), int(8, U64)];
    let l = fb.call(left, Callee::Extern(env.alloc), args.clone(), Some(w));
    fb.assign(l, q, Rvalue::Use(copy_local(k)));
    fb.goto(l, join);
    let r = fb.call(right, Callee::Extern(env.alloc), args, Some(w));
    fb.assign(r, q, Rvalue::Use(copy_local(w)));
    fb.goto(r, join);
    fill(&mut fb, join, w, 16);
    store(&mut fb, join, field(w, env.pair, 0), int(1, I64));
    fb.assign(join, x, Rvalue::Use(copy_place(field(q, env.pair, 1))));
    store(&mut fb, join, field(w, env.pair, 1), int(2, I64));
    fb.ret(join, copy_local(x));
    env.pb.add(fb.finish());
    let p = env.pb.finish();
    let mut opt = p.clone();
    let aggs = opt.aggs.clone();
    let allocator = Allocator::find(&opt);
    run(&aggs, allocator, &mut opt.funcs[0]);
    let fill_at_join = |p: &Program| {
        let stmts = &p.funcs[0].blocks[join.0 as usize].stmts;
        stmts.iter().any(|s| matches!(s, Stmt::MemSet { .. }))
    };
    assert!(fill_at_join(&opt), "{opt}");
    assert_eq!(run_main(&p, 5), 0);
    assert_eq!(run_main(&opt, 5), 0);
}

#[test]
fn locals_whose_address_is_taken_are_not_tracked() {
    // `r = &q` before the allocation, then `q = w` (or `q` is `w` itself); `*r = &d` repoints
    // `q` unseen, so the stores through `q` write `d` and the object's fill must stay.
    for copy in [true, false] {
        let env = env();
        let mut fb = FuncBuilder::export("main", &[I64], I64);
        let w = fb.local(Ptr);
        let q = if copy { fb.local(Ptr) } else { w };
        let (r, d, dp) = (fb.local(Ptr), fb.local(Ty::Agg(env.pair)), fb.local(Ptr));
        let b = fb.block();
        fb.assign(b, r, Rvalue::AddrOf(Place::local(q)));
        fb.assign(b, dp, Rvalue::AddrOf(Place::local(d)));
        let b = env.alloc(&mut fb, b, w, 16);
        if copy {
            fb.assign(b, q, Rvalue::Use(copy_local(w)));
        }
        store(&mut fb, b, deref(r, Ptr), copy_local(dp));
        store(&mut fb, b, field(q, env.pair, 0), int(5, I64));
        store(&mut fb, b, field(q, env.pair, 1), int(2, I64));
        let pair = env.pair;
        check(env.finish(fb, b, w, pair), false);
    }
}
