//! Unit tests for known vtables on hand-built VIR: a new object's header is forwarded to the
//! load of it, the method slot of the static vtable is folded, and `constfold` then calls the
//! method directly; each program runs in the interpreter before and after.

use super::*;
use crate::interp::{Interp, Memory, Trap};
use crate::testkit::builder::*;
use crate::testkit::validate::assert_valid;
use velt_vir::vir::Ty::*;
use velt_vir::vir::{
    AggId, BinOp, BlockId, ExternFn, ExternId, FuncId, Local, Program, Proj, Rvalue, StaticId, Stmt,
};

/// A heap; other extern calls return 0.
struct Heap;

impl crate::interp::Host for Heap {
    fn call(&mut self, ext: &ExternFn, args: &[u64], mem: &mut Memory) -> Result<u64, Trap> {
        match ext.symbol.as_str() {
            "velt_rt_alloc" => mem.alloc_heap(args[0], args[1]),
            _ => Ok(0),
        }
    }
}

fn run_main(p: &Program, n: u64) -> u64 {
    Interp::new(p, Heap)
        .call_symbol("main", &[n])
        .expect("program runs")
}

struct Env {
    pb: ProgramBuilder,
    alloc: ExternId,
    keep: ExternId,
    /// `Sq object { vtable: ptr, side: i64 }`
    sq: AggId,
    /// `Shape object { vtable: ptr }`: the base class's view.
    shape: AggId,
    /// `Sq.area(this) -> i64`
    area: FuncId,
    /// Sq's vtable: 8 bytes of header, then `area`.
    vtable: StaticId,
}

fn env() -> Env {
    let mut pb = ProgramBuilder::new();
    let alloc = pb.ext("velt_rt_alloc", &[U64, U64], Ptr, false);
    pb.ext("velt_rt_free", &[Ptr, U64, U64], Unit, false);
    let keep = pb.ext("keep", &[Ptr], Unit, false);
    let sq = pb.agg("Sq object", 16, 8, &[(Ptr, 0), (I64, 8)]);
    let shape = pb.agg("Shape object", 8, 8, &[(Ptr, 0)]);
    let mut fb = FuncBuilder::internal("area", &[Ptr], I64);
    let (this, r) = (fb.param(0), fb.local(I64));
    let b = fb.block();
    let side = copy_place(field(this, sq, 1));
    fb.assign(b, r, bin(BinOp::Mul, side.clone(), side));
    fb.ret(b, copy_local(r));
    let area = pb.add(fb.finish());
    let vtable = pb.stat_with(&[0; 16], 8, vec![(8, Const::Func(area))]);
    Env {
        pb,
        alloc,
        keep,
        sq,
        shape,
        area,
        vtable,
    }
}

fn field(w: Local, obj: AggId, k: u32) -> Place {
    Place {
        local: w,
        proj: vec![Proj::Deref(Ty::Agg(obj)), Proj::Field(k)],
    }
}

impl Env {
    /// `p = new Sq(n)`: allocation, fill, header, field; returns the continuation.
    fn new_sq(&self, fb: &mut FuncBuilder, b: BlockId, p: Local, n: Operand) -> BlockId {
        let args = vec![int(16, U64), int(8, U64)];
        let b = fb.call(b, Callee::Extern(self.alloc), args, Some(p));
        let fill = Stmt::MemSet {
            dst: copy_local(p),
            byte: int(0, U8),
            len: int(16, U64),
        };
        fb.push(b, fill);
        let header = Operand::Const(Const::Static(self.vtable), Ptr);
        fb.push(b, Stmt::Assign(field(p, self.sq, 0), Rvalue::Use(header)));
        fb.push(b, Stmt::Assign(field(p, self.sq, 1), Rvalue::Use(n)));
        b
    }

    /// `v = p.vtable (as Shape); m = *(v + 8); r = m(p)`; returns (continuation, r).
    fn virtual_area(&self, fb: &mut FuncBuilder, b: BlockId, p: Local) -> (BlockId, Local) {
        let (v, s, m, r) = (fb.local(Ptr), fb.local(Ptr), fb.local(Ptr), fb.local(I64));
        fb.assign(b, v, Rvalue::Use(copy_place(field(p, self.shape, 0))));
        fb.assign(b, s, bin(BinOp::PtrAdd, copy_local(v), int(8, I64)));
        fb.assign(b, m, Rvalue::Use(copy_place(deref(s, Ptr))));
        let callee = Callee::Ptr {
            target: copy_local(m),
            params: vec![Ptr],
            ret: I64,
        };
        let b = fb.call(b, callee, vec![copy_local(p)], Some(r));
        (b, r)
    }
}

/// `main(n)`: `p = new Sq(n)`, `between`, then the virtual call `p.area()`.
fn program(between: impl FnOnce(&Env, &mut FuncBuilder, BlockId, Local) -> BlockId) -> Program {
    let mut env = env();
    let mut fb = FuncBuilder::export("main", &[I64], I64);
    let p = fb.local(Ptr);
    let b = fb.block();
    let n = copy_local(fb.param(0));
    let b = env.new_sq(&mut fb, b, p, n);
    let b = between(&env, &mut fb, b, p);
    let (b, r) = env.virtual_area(&mut fb, b, p);
    fb.ret(b, copy_local(r));
    env.pb.add(fb.finish());
    env.pb.finish()
}

/// Run the pass and `constfold` on `main`; whether its call became a direct call to `area`.
fn devirtualized(original: &Program) -> bool {
    let mut p = original.clone();
    let main = p.funcs.len() - 1;
    let (aggs, statics) = (p.aggs.clone(), p.statics.clone());
    let allocator = Allocator::find(&p);
    run(&aggs, &statics, allocator, &mut p.funcs[main]);
    let signatures = crate::callgraph::signatures(&p);
    crate::constfold::run(&signatures, &mut p.funcs[main]);
    assert_valid(&p);
    for n in [0, 3, 7] {
        assert_eq!(run_main(original, n), run_main(&p, n), "n = {n}\n{p}");
    }
    let area = env().area;
    p.funcs[main]
        .blocks
        .iter()
        .any(|b| matches!(&b.term, Terminator::Call { callee: Callee::Func(f), .. } if *f == area))
}

#[test]
fn a_call_on_a_new_object_goes_to_its_class_method() {
    assert!(devirtualized(&program(|_, _, b, _| b)));
}

#[test]
fn a_header_seen_after_the_object_escapes_is_not_forwarded() {
    let p =
        program(|env, fb, b, p| fb.call(b, Callee::Extern(env.keep), vec![copy_local(p)], None));
    assert!(!devirtualized(&p));
}

#[test]
fn a_header_overwritten_with_an_unknown_pointer_is_not_forwarded() {
    let p = program(|env, fb, b, p| {
        let other = fb.local(Ptr);
        fb.assign(b, other, Rvalue::Use(copy_place(field(p, env.sq, 0))));
        let shifted = fb.local(Ptr);
        fb.assign(
            b,
            shifted,
            bin(BinOp::PtrAdd, copy_local(other), int(0, I64)),
        );
        let b = fb.call(b, Callee::Extern(env.keep), vec![int(0, Ptr)], None);
        fb.push(
            b,
            Stmt::Assign(field(p, env.sq, 0), Rvalue::Use(copy_local(shifted))),
        );
        b
    });
    // The header now holds a pointer computed at run time (the same vtable here): the pass
    // must not assume it.
    assert!(!devirtualized(&p));
}

#[test]
fn a_known_static_slot_is_folded_without_an_allocation() {
    // `s = vtable + 8; m = *s; m(p)` with `p` a parameter.
    let mut env = env();
    let mut fb = FuncBuilder::export("main", &[I64], I64);
    let (p, s, m, r) = (fb.local(Ptr), fb.local(Ptr), fb.local(Ptr), fb.local(I64));
    let b = fb.block();
    let n = copy_local(fb.param(0));
    let b = env.new_sq(&mut fb, b, p, n);
    let b = fb.call(b, Callee::Extern(env.keep), vec![copy_local(p)], None);
    let vt = Operand::Const(Const::Static(env.vtable), Ptr);
    fb.assign(b, s, bin(BinOp::PtrAdd, vt, int(8, I64)));
    fb.assign(b, m, Rvalue::Use(copy_place(deref(s, Ptr))));
    let callee = Callee::Ptr {
        target: copy_local(m),
        params: vec![Ptr],
        ret: I64,
    };
    let b = fb.call(b, callee, vec![copy_local(p)], Some(r));
    fb.ret(b, copy_local(r));
    env.pb.add(fb.finish());
    assert!(devirtualized(&env.pb.finish()));
}

#[test]
fn a_slot_varying_by_path_or_without_a_relocation_is_kept() {
    // `s = n < 3 ? vtable + 8 : vtable` (two offsets), or `s = vtable` (no relocation at 0).
    for two_paths in [true, false] {
        let mut env = env();
        let mut fb = FuncBuilder::export("main", &[I64], I64);
        let (s, m, r, c) = (fb.local(Ptr), fb.local(Ptr), fb.local(I64), fb.local(Bool));
        let b = fb.block();
        let vt = Operand::Const(Const::Static(env.vtable), Ptr);
        fb.assign(b, s, bin(BinOp::PtrAdd, vt.clone(), int(0, I64)));
        let b = if two_paths {
            let n = copy_local(fb.param(0));
            fb.assign(b, c, bin(BinOp::Lt, n, int(3, I64)));
            let (then, join) = (fb.block(), fb.block());
            fb.branch(b, c, then, join);
            fb.assign(then, s, bin(BinOp::PtrAdd, vt, int(8, I64)));
            fb.goto(then, join);
            join
        } else {
            b
        };
        fb.assign(b, m, Rvalue::Use(copy_place(deref(s, Ptr))));
        fb.assign(b, r, Rvalue::Cast(copy_local(m), I64));
        fb.ret(b, copy_local(r));
        env.pb.add(fb.finish());
        let mut p = env.pb.finish();
        let (aggs, statics) = (p.aggs.clone(), p.statics.clone());
        let main = p.funcs.len() - 1;
        assert!(!run(&aggs, &statics, None, &mut p.funcs[main]), "{p}");
    }
}

#[test]
fn a_header_seen_where_two_allocations_meet_is_not_forwarded() {
    // `p = n < 3 ? new Sq(n) : new Other(n)` (the same layout, another vtable), then
    // `p.area()` where both paths meet: the call must not go to `Sq.area`.
    let mut env = env();
    let mut fb = FuncBuilder::internal("other_area", &[Ptr], I64);
    let (this, r) = (fb.param(0), fb.local(I64));
    let b = fb.block();
    let side = copy_place(field(this, env.sq, 1));
    fb.assign(b, r, bin(BinOp::Add, side, int(100, I64)));
    fb.ret(b, copy_local(r));
    let other_area = env.pb.add(fb.finish());
    let other = env
        .pb
        .stat_with(&[0; 16], 8, vec![(8, Const::Func(other_area))]);
    let mut fb = FuncBuilder::export("main", &[I64], I64);
    let (p, c) = (fb.local(Ptr), fb.local(Bool));
    let b = fb.block();
    let n = copy_local(fb.param(0));
    fb.assign(b, c, bin(BinOp::Lt, n.clone(), int(3, I64)));
    let (left, right, join) = (fb.block(), fb.block(), fb.block());
    fb.branch(b, c, left, right);
    let l = env.new_sq(&mut fb, left, p, n.clone());
    fb.goto(l, join);
    let r = env.new_sq(&mut fb, right, p, n);
    let header = Operand::Const(Const::Static(other), Ptr);
    fb.push(r, Stmt::Assign(field(p, env.sq, 0), Rvalue::Use(header)));
    fb.goto(r, join);
    let (b, res) = env.virtual_area(&mut fb, join, p);
    fb.ret(b, copy_local(res));
    env.pb.add(fb.finish());
    let p = env.pb.finish();
    assert!(!devirtualized(&p));
    assert_eq!(run_main(&p, 5), 105);
}
