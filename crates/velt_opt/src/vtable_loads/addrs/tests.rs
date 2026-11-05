//! The tracked slots must decide exactly what every slot would (module docs): shapes where a
//! fact reaches a load only through a join, a loop or an aggregate, or is lost to a store through
//! a pointer. Every shape, and the random functions of `random`, is compared with a solver that
//! tracks all slots.

use super::super::slot_loads;
use super::*;
use crate::testkit::builder::*;
use velt_vir::vir::Ty::*;
use velt_vir::vir::{AggId, BlockId, Callee, ExternId};

mod random;

use random::dense_loads;

struct Env {
    pb: ProgramBuilder,
    keep: ExternId,
    /// `{ a: ptr, b: ptr }`
    pair: AggId,
    /// `{ head: ptr, inner: pair }`: as many fields as `pair`, one of them nested.
    nested: AggId,
    /// `{ a: ptr, b: ptr, c: ptr }`
    triple: AggId,
    /// Relocations at 8 and 16.
    vtable: StaticId,
    /// A relocation at 8.
    other: StaticId,
    /// No relocation.
    plain: StaticId,
}

fn env() -> Env {
    let mut pb = ProgramBuilder::new();
    let keep = pb.ext("keep", &[Ptr], Ptr, false);
    let pair = pb.agg("pair", 16, 8, &[(Ptr, 0), (Ptr, 8)]);
    let nested = pb.agg("nested", 24, 8, &[(Ptr, 0), (Ty::Agg(pair), 8)]);
    let triple = pb.agg("triple", 24, 8, &[(Ptr, 0), (Ptr, 8), (Ptr, 16)]);
    let mut fb = FuncBuilder::internal("method", &[], Unit);
    let b = fb.block();
    fb.ret(b, Operand::Const(Const::Unit, Unit));
    let method = pb.add(fb.finish());
    let func = Const::Func(method);
    let vtable = pb.stat_with(&[0; 24], 8, vec![(8, func.clone()), (16, func.clone())]);
    let other = pb.stat_with(&[0; 16], 8, vec![(8, func)]);
    let plain = pb.stat(&[0; 24], 8);
    Env {
        pb,
        keep,
        pair,
        nested,
        triple,
        vtable,
        other,
        plain,
    }
}

fn address(id: StaticId) -> Operand {
    Operand::Const(Const::Static(id), Ptr)
}

/// `s = x + 8; m = *s`
fn load_slot(fb: &mut FuncBuilder, b: BlockId, x: Operand) {
    let (s, m) = (fb.local(Ptr), fb.local(Ptr));
    fb.assign(b, s, bin(BinOp::PtrAdd, x, int(8, I64)));
    fb.assign(b, m, Rvalue::Use(copy_place(deref(s, Ptr))));
}

/// The loads folded in `main(c: bool, q: ptr)` as `body` builds it (the closure gets its entry
/// block and ends every block).
fn folds(body: impl FnOnce(&Env, &mut FuncBuilder, BlockId)) -> usize {
    let mut env = env();
    let mut fb = FuncBuilder::export("main", &[Bool, Ptr], Unit);
    let b = fb.block();
    body(&env, &mut fb, b);
    let f = env.pb.add(fb.finish());
    let p = env.pb.finish();
    let func = &p.funcs[f.0 as usize];
    let loads = slot_loads(&p.aggs, &p.statics, func);
    assert_eq!(loads, dense_loads(&p, func), "{func:?}");
    loads.len()
}

fn end(fb: &mut FuncBuilder, b: BlockId) {
    fb.ret(b, Operand::Const(Const::Unit, Unit));
}

/// `a = vtable; if c { x = a } else { x = right }; (x + 8).*`
fn through_a_join(right: impl FnOnce(&Env, Local) -> Operand) -> usize {
    folds(|env, fb, b| {
        let (a, x) = (fb.local(Ptr), fb.local(Ptr));
        fb.assign(b, a, Rvalue::Use(address(env.vtable)));
        let (then, els, join) = (fb.block(), fb.block(), fb.block());
        fb.branch(b, fb.param(0), then, els);
        fb.assign(then, x, Rvalue::Use(copy_local(a)));
        fb.goto(then, join);
        fb.assign(els, x, Rvalue::Use(right(env, a)));
        fb.goto(els, join);
        load_slot(fb, join, copy_local(x));
        end(fb, join);
    })
}

#[test]
fn a_slot_reached_only_through_a_join() {
    assert_eq!(through_a_join(|_, a| copy_local(a)), 1);
    assert_eq!(through_a_join(|env, _| address(env.vtable)), 1);
    assert_eq!(through_a_join(|env, _| address(env.other)), 0);
    assert_eq!(through_a_join(|_, _| int(0, Ptr)), 0);
}

#[test]
fn a_source_unassigned_on_one_path_does_not_spoil_the_join() {
    // `if c { x = vtable }; y = x; (y + 8).*`: `x` is no load's pointer, and must be exact for
    // `y` to be known: not tracking it would read as "varies".
    let n = folds(|env, fb, b| {
        let (x, y) = (fb.local(Ptr), fb.local(Ptr));
        let (then, join) = (fb.block(), fb.block());
        fb.branch(b, fb.param(0), then, join);
        fb.assign(then, x, Rvalue::Use(address(env.vtable)));
        fb.goto(then, join);
        fb.assign(join, y, Rvalue::Use(copy_local(x)));
        load_slot(fb, join, copy_local(y));
        end(fb, join);
    });
    assert_eq!(n, 1);
}

/// `p = vtable; loop { x = p; (x + 8).*; p = next(x); if c continue }`
fn in_a_loop(next: impl FnOnce(&Env, &mut FuncBuilder, Local) -> Rvalue) -> usize {
    folds(|env, fb, b| {
        let (p, x) = (fb.local(Ptr), fb.local(Ptr));
        fb.assign(b, p, Rvalue::Use(address(env.vtable)));
        let (head, exit) = (fb.block(), fb.block());
        fb.goto(b, head);
        fb.assign(head, x, Rvalue::Use(copy_local(p)));
        load_slot(fb, head, copy_local(x));
        let rv = next(env, fb, x);
        fb.assign(head, p, rv);
        fb.branch(head, fb.param(0), head, exit);
        end(fb, exit);
    })
}

#[test]
fn a_slot_around_a_loop() {
    assert_eq!(in_a_loop(|_, _, x| Rvalue::Use(copy_local(x))), 1);
    // The back edge brings another value: only a second visit of the head can see it.
    let moved = |_: &Env, _: &mut FuncBuilder, x| bin(BinOp::PtrAdd, copy_local(x), int(8, I64));
    assert_eq!(in_a_loop(moved), 0);
    assert_eq!(
        in_a_loop(|_, fb, _| Rvalue::Use(copy_local(fb.param(1)))),
        0
    );
}

#[test]
fn a_store_through_a_pointer_forgets_it() {
    // The store goes through the slot of the load, or through a slot it was copied from.
    for through_source in [false, true] {
        let n = folds(|env, fb, b| {
            let (a, s, m) = (fb.local(Ptr), fb.local(Ptr), fb.local(Ptr));
            fb.assign(b, a, Rvalue::Use(address(env.vtable)));
            if through_source {
                fb.push(b, Stmt::Assign(deref(a, Ptr), Rvalue::Use(int(0, Ptr))));
            }
            fb.assign(b, s, bin(BinOp::PtrAdd, copy_local(a), int(8, I64)));
            if !through_source {
                fb.push(b, Stmt::Assign(deref(s, Ptr), Rvalue::Use(int(0, Ptr))));
            }
            fb.assign(b, m, Rvalue::Use(copy_place(deref(s, Ptr))));
            end(fb, b);
        });
        assert_eq!(n, 0, "through its source: {through_source}");
    }
}

#[test]
fn a_call_result_is_not_known_on_its_path() {
    let n = folds(|env, fb, b| {
        let x = fb.local(Ptr);
        fb.assign(b, x, Rvalue::Use(address(env.vtable)));
        let (then, join) = (fb.block(), fb.block());
        fb.branch(b, fb.param(0), then, join);
        let args = vec![copy_local(x)];
        let after = fb.call(then, Callee::Extern(env.keep), args, Some(x));
        fb.goto(after, join);
        load_slot(fb, join, copy_local(x));
        end(fb, join);
    });
    assert_eq!(n, 0);
}

/// `a = (vtable, q)`, then `pick` (in the same block) names the pointer to load through.
fn through_an_aggregate(pick: impl FnOnce(&mut FuncBuilder, AggId, Local) -> Place) -> usize {
    folds(|env, fb, b| {
        let a = fb.local(Ty::Agg(env.pair));
        let x = fb.local(Ptr);
        let q = copy_local(fb.param(1));
        let fields = vec![address(env.vtable), q];
        fb.assign(b, a, Rvalue::Aggregate(env.pair, fields));
        let place = pick(fb, env.pair, a);
        fb.assign(b, x, Rvalue::Use(copy_place(place)));
        load_slot(fb, b, copy_local(x));
        end(fb, b);
    })
}

#[test]
fn a_field_of_an_aggregate_is_tracked_through_copies_and_swaps() {
    assert_eq!(through_an_aggregate(|_, _, a| field(a, 0)), 1);
    assert_eq!(through_an_aggregate(|_, _, a| field(a, 1)), 0);
    let copy = |fb: &mut FuncBuilder, pair: AggId, a: Local| {
        let c = fb.local(Ty::Agg(pair));
        fb.assign(BlockId(0), c, Rvalue::Use(copy_local(a)));
        field(c, 0)
    };
    assert_eq!(through_an_aggregate(copy), 1);
    // `a = (a.1, a.0)` reads both before it writes either.
    let swap = |k: u32| {
        move |fb: &mut FuncBuilder, pair: AggId, a: Local| {
            let fields = vec![copy_place(field(a, 1)), copy_place(field(a, 0))];
            fb.assign(BlockId(0), a, Rvalue::Aggregate(pair, fields));
            field(a, k)
        }
    };
    assert_eq!(through_an_aggregate(swap(1)), 1);
    assert_eq!(through_an_aggregate(swap(0)), 0);
}

#[test]
fn a_read_in_a_terminator_is_folded() {
    // The only read of the block it ends: nothing else makes the walk look at that block.
    let n = folds(|env, fb, b| {
        let (s, m) = (fb.local(Ptr), fb.local(Ptr));
        fb.assign(b, s, bin(BinOp::PtrAdd, address(env.vtable), int(8, I64)));
        fb.assign(b, m, Rvalue::Use(copy_place(deref(s, Ptr))));
        let call = fb.block();
        fb.goto(b, call);
        let args = vec![copy_place(deref(s, Ptr))];
        let after = fb.call(call, Callee::Extern(env.keep), args, None);
        end(fb, after);
    });
    assert_eq!(n, 2);
}

#[test]
fn a_load_through_a_loaded_pointer_needs_no_state() {
    // `s` is a candidate and folds; `x`, loaded from it, is one too but never holds a static, so
    // nothing tracks it, and a plain static is no reason to track `y` either.
    let mut env = env();
    let mut fb = FuncBuilder::export("main", &[], Unit);
    let b = fb.block();
    let (s, x, m, y, n) = (
        fb.local(Ptr),
        fb.local(Ptr),
        fb.local(Ptr),
        fb.local(Ptr),
        fb.local(Ptr),
    );
    fb.assign(b, s, bin(BinOp::PtrAdd, address(env.vtable), int(8, I64)));
    fb.assign(b, x, Rvalue::Use(copy_place(deref(s, Ptr))));
    fb.assign(b, m, Rvalue::Use(copy_place(deref(x, Ptr))));
    fb.assign(b, y, Rvalue::Use(address(env.plain)));
    fb.assign(b, n, Rvalue::Use(copy_place(deref(y, Ptr))));
    end(&mut fb, b);
    let f = env.pb.add(fb.finish());
    let p = env.pb.finish();
    let func = &p.funcs[f.0 as usize];
    let addrs = Addrs::of(&p.aggs, &p.statics, func).expect("a load can fold");
    assert_eq!(addrs.tracked.iter().flatten().count(), 1);
    assert_eq!(slot_loads(&p.aggs, &p.statics, func).len(), 1);
}

#[test]
fn nothing_to_fold_needs_no_analysis() {
    // A vtable flows into `s`, but no load of a pointer goes through it.
    let mut env = env();
    let mut fb = FuncBuilder::export("main", &[], Unit);
    let b = fb.block();
    let (s, v) = (fb.local(Ptr), fb.local(I64));
    fb.assign(b, s, Rvalue::Use(address(env.vtable)));
    fb.assign(b, v, Rvalue::Use(copy_place(deref(s, I64))));
    end(&mut fb, b);
    let f = env.pb.add(fb.finish());
    let p = env.pb.finish();
    assert!(Addrs::of(&p.aggs, &p.statics, &p.funcs[f.0 as usize]).is_none());
}

/// `x = vtable + 8; m = *x`, with `x`'s address taken after the load when `taken`.
fn with_its_address(taken: bool) -> usize {
    folds(|env, fb, b| {
        let (x, m, q) = (fb.local(Ptr), fb.local(Ptr), fb.local(Ptr));
        fb.assign(b, x, bin(BinOp::PtrAdd, address(env.vtable), int(8, I64)));
        fb.assign(b, m, Rvalue::Use(copy_place(deref(x, Ptr))));
        if taken {
            fb.assign(b, q, Rvalue::AddrOf(Place::local(x)));
        }
        end(fb, b);
    })
}

#[test]
fn a_slot_whose_address_is_taken_is_never_known() {
    assert_eq!(with_its_address(false), 1);
    // A write through `q` could change it, wherever the address is taken.
    assert_eq!(with_its_address(true), 0);
    // Also as the source of another slot, and as an aggregate through a field's address.
    let copied = folds(|env, fb, b| {
        let (x, q, y) = (fb.local(Ptr), fb.local(Ptr), fb.local(Ptr));
        fb.assign(b, x, Rvalue::Use(address(env.vtable)));
        fb.assign(b, q, Rvalue::AddrOf(Place::local(x)));
        fb.assign(b, y, Rvalue::Use(copy_local(x)));
        load_slot(fb, b, copy_local(y));
        end(fb, b);
    });
    assert_eq!(copied, 0);
    let field_taken = folds(|env, fb, b| {
        let (a, q, x) = (fb.local(Ty::Agg(env.pair)), fb.local(Ptr), fb.local(Ptr));
        let fields = vec![address(env.vtable), int(0, Ptr)];
        fb.assign(b, a, Rvalue::Aggregate(env.pair, fields));
        fb.assign(b, q, Rvalue::AddrOf(field(a, 1)));
        fb.assign(b, x, Rvalue::Use(copy_place(field(a, 0))));
        load_slot(fb, b, copy_local(x));
        end(fb, b);
    });
    assert_eq!(field_taken, 0);
}

#[test]
fn an_unreachable_block_does_not_reach_a_join() {
    // `x = vtable`, then the join; a block nothing jumps to gives `x` another static and loads
    // through it, and a dead cycle around it does the same.
    let n = folds(|env, fb, b| {
        let x = fb.local(Ptr);
        fb.assign(b, x, Rvalue::Use(address(env.vtable)));
        let (join, dead, dead_too) = (fb.block(), fb.block(), fb.block());
        fb.goto(b, join);
        fb.assign(dead, x, Rvalue::Use(address(env.other)));
        load_slot(fb, dead, copy_local(x));
        fb.branch(dead, fb.param(0), join, dead_too);
        fb.assign(dead_too, x, Rvalue::Use(copy_local(fb.param(1))));
        fb.goto(dead_too, dead);
        load_slot(fb, join, copy_local(x));
        end(fb, join);
    });
    assert_eq!(n, 1);
}

/// `x = vtable + 8; if c goto a else goto b`, with `a` and `b` jumping to each other (neither
/// dominates the other, so no order sees all of a loop's values in one sweep), both loading
/// through `x`, then `x` loaded at the exit. `b` runs `change` on `x` after its load.
fn around_an_irreducible_cycle(
    change: impl FnOnce(&Env, &mut FuncBuilder, BlockId, Local),
) -> usize {
    folds(|env, fb, b| {
        let (x, m) = (fb.local(Ptr), fb.local(Ptr));
        fb.assign(b, x, bin(BinOp::PtrAdd, address(env.vtable), int(8, I64)));
        let (first, second, exit) = (fb.block(), fb.block(), fb.block());
        fb.branch(b, fb.param(0), first, second);
        fb.assign(first, m, Rvalue::Use(copy_place(deref(x, Ptr))));
        fb.branch(first, fb.param(0), second, exit);
        fb.assign(second, m, Rvalue::Use(copy_place(deref(x, Ptr))));
        change(env, fb, second, x);
        fb.branch(second, fb.param(0), first, exit);
        fb.assign(exit, m, Rvalue::Use(copy_place(deref(x, Ptr))));
        end(fb, exit);
    })
}

#[test]
fn a_slot_around_an_irreducible_cycle() {
    assert_eq!(around_an_irreducible_cycle(|_, _, _, _| {}), 3);
    // The other value gets to both blocks of the cycle and out of it, by either way in.
    let unknown = |_: &Env, fb: &mut FuncBuilder, b, x| {
        fb.assign(b, x, Rvalue::Use(copy_local(fb.param(1))));
    };
    assert_eq!(around_an_irreducible_cycle(unknown), 0);
    let same = |env: &Env, fb: &mut FuncBuilder, b, x| {
        fb.assign(b, x, bin(BinOp::PtrAdd, address(env.vtable), int(8, I64)));
    };
    assert_eq!(around_an_irreducible_cycle(same), 3);
}

/// `s = base + offset; m = *s` through a place that views `s` as `proj`.
fn through_projection(base: StaticId, offset: i128, proj: Vec<Proj>) -> usize {
    let mut env = env();
    let mut fb = FuncBuilder::export("main", &[], Unit);
    let b = fb.block();
    let (s, m) = (fb.local(Ptr), fb.local(Ptr));
    let start = Operand::Const(Const::Static(base), Ptr);
    fb.assign(b, s, bin(BinOp::PtrAdd, start, int(offset, I64)));
    fb.assign(b, m, Rvalue::Use(copy_place(Place { local: s, proj })));
    end(&mut fb, b);
    let f = env.pb.add(fb.finish());
    let p = env.pb.finish();
    let func = &p.funcs[f.0 as usize];
    let loads = slot_loads(&p.aggs, &p.statics, func);
    assert_eq!(loads, dense_loads(&p, func), "{func:?}");
    loads.len()
}

#[test]
fn a_load_through_a_field_projection_reads_at_the_fields_offset() {
    let e = env();
    let (vtable, other) = (e.vtable, e.other);
    let view = |id| Proj::Deref(Ty::Agg(id));
    // Relocations at 8 and 16 in `vtable`, at 8 in `other`.
    let at = through_projection;
    assert_eq!(at(vtable, 0, vec![view(e.pair), Proj::Field(0)]), 0);
    assert_eq!(at(vtable, 0, vec![view(e.pair), Proj::Field(1)]), 1);
    assert_eq!(at(vtable, 8, vec![view(e.pair), Proj::Field(1)]), 1);
    assert_eq!(at(other, 8, vec![view(e.pair), Proj::Field(1)]), 0);
    assert_eq!(at(vtable, 0, vec![view(e.triple), Proj::Field(2)]), 1);
    // Through a nested field: `nested.inner.b` is at 16, `nested.inner.a` at 8.
    let inner = |k| vec![view(e.nested), Proj::Field(1), Proj::Field(k)];
    assert_eq!(at(other, 0, inner(0)), 1);
    assert_eq!(at(other, 0, inner(1)), 0);
    // A field that is an aggregate is no pointer to load, and a pointer the place goes on
    // through reads elsewhere.
    assert_eq!(at(vtable, 0, vec![view(e.nested), Proj::Field(1)]), 0);
    let through = vec![view(e.pair), Proj::Field(1), Proj::Deref(Ptr)];
    assert_eq!(at(vtable, 0, through), 0);
    // A reinterpreting view reads the same bytes.
    let cast = vec![view(e.pair), Proj::Cast(e.triple), Proj::Field(2)];
    assert_eq!(at(vtable, 0, cast), 1);
}

#[test]
fn aggregates_of_another_shape_forget_what_they_copy() {
    // `a = (vtable, q)`, then `b = a`, then `x = b.0`, for `b` of each shape: as many fields
    // copies them one by one, another count is an unknown value.
    let copy_to = |shape: fn(&Env) -> AggId| {
        folds(|env, fb, b| {
            let a = fb.local(Ty::Agg(env.pair));
            let c = fb.local(Ty::Agg(shape(env)));
            let x = fb.local(Ptr);
            let fields = vec![address(env.vtable), copy_local(fb.param(1))];
            fb.assign(b, a, Rvalue::Aggregate(env.pair, fields));
            fb.assign(b, c, Rvalue::Use(copy_local(a)));
            fb.assign(b, x, Rvalue::Use(copy_place(field(c, 0))));
            load_slot(fb, b, copy_local(x));
            end(fb, b);
        })
    };
    assert_eq!(copy_to(|env| env.pair), 1);
    assert_eq!(copy_to(|env| env.nested), 1);
    assert_eq!(copy_to(|env| env.triple), 0);
    // A write inside a nested field forgets the whole local.
    let nested_write = folds(|env, fb, b| {
        let a = fb.local(Ty::Agg(env.nested));
        let x = fb.local(Ptr);
        let fields = vec![address(env.vtable), int(0, Ptr)];
        fb.assign(b, a, Rvalue::Aggregate(env.nested, fields));
        let inner = Place {
            local: a,
            proj: vec![Proj::Field(1), Proj::Field(0)],
        };
        fb.push(b, Stmt::Assign(inner, Rvalue::Use(int(0, Ptr))));
        fb.assign(b, x, Rvalue::Use(copy_place(field(a, 0))));
        load_slot(fb, b, copy_local(x));
        end(fb, b);
    });
    assert_eq!(nested_write, 0);
}
