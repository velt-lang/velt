//! The tracked slots must decide exactly what every slot would (module docs): shapes where a
//! fact reaches a load only through a join, a loop or an aggregate, or is lost to a store through
//! a pointer, and random functions compared with a solver that tracks all slots.

use super::super::{slot_loads, slot_read, Load};
use super::*;
use crate::testkit::builder::*;
use velt_vir::vir::Ty::*;
use velt_vir::vir::{AggId, BlockId, Callee, ExternId, Program};

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

/// The analysis before it was sparse: every slot of every candidate, in a last-in-first-out
/// worklist, which is what the tracked slots and the order must reproduce.
mod dense {
    use super::*;

    pub struct Dense {
        pub slots: Vec<Option<Slots>>,
        pub entry: Vec<Option<Vec<Lat>>>,
    }

    impl Dense {
        pub fn of(aggs: &[AggLayout], statics: &[StaticData], func: &Function) -> Option<Dense> {
            let cands = candidates(statics, func)?;
            let mut slots = vec![None; func.locals.len()];
            let mut n = 0;
            for (i, decl) in func.locals.iter().enumerate() {
                if !cands[i] {
                    continue;
                }
                let (s, len) = match decl.ty {
                    Ty::Ptr => (Slots::Ptr(n), 1),
                    Ty::Agg(id) => match aggs.get(id.0 as usize) {
                        Some(a) => (Slots::Agg(n, a.fields.len()), a.fields.len()),
                        None => continue,
                    },
                    _ => continue,
                };
                slots[i] = Some(s);
                n += len;
            }
            let mut d = Dense {
                slots,
                entry: vec![None; func.blocks.len()],
            };
            d.entry[0] = Some(vec![Lat::None; n]);
            let mut queued = vec![false; func.blocks.len()];
            let mut work = vec![0];
            queued[0] = true;
            while let Some(b) = work.pop() {
                queued[b] = false;
                let Some(mut st) = d.entry[b].clone() else {
                    continue;
                };
                let block = &func.blocks[b];
                for s in &block.stmts {
                    d.step(&mut st, s);
                }
                if let Terminator::Call { dest: Some(l), .. } = &block.term {
                    d.clobber(&mut st, l.local);
                }
                for next in successors(&block.term) {
                    let next = next.0 as usize;
                    if meet_into(&mut d.entry[next], &st)
                        && !std::mem::replace(&mut queued[next], true)
                    {
                        work.push(next);
                    }
                }
            }
            Some(d)
        }

        fn slot(&self, p: &Place) -> Option<usize> {
            match (
                self.slots.get(p.local.0 as usize).copied().flatten()?,
                &p.proj[..],
            ) {
                (Slots::Ptr(i), []) => Some(i),
                (Slots::Agg(i, len), [Proj::Field(k)]) if (*k as usize) < len => {
                    Some(i + *k as usize)
                }
                _ => None,
            }
        }

        fn value(&self, st: &[Lat], op: &Operand) -> Lat {
            match op {
                Operand::Const(Const::Static(id), Ty::Ptr) => Lat::Known(*id, 0),
                Operand::Copy(p) => self.slot(p).map_or(Lat::Varies, |i| st[i]),
                _ => Lat::Varies,
            }
        }

        fn eval(&self, st: &[Lat], rv: &Rvalue) -> Lat {
            match rv {
                Rvalue::Use(op) => self.value(st, op),
                Rvalue::Binary(BinOp::PtrAdd, a, Operand::Const(Const::Int(c), _)) => {
                    match self.value(st, a) {
                        Lat::Known(id, off) => Lat::Known(id, off + c),
                        other => other,
                    }
                }
                _ => Lat::Varies,
            }
        }

        fn clobber(&self, st: &mut [Lat], l: Local) {
            let range = match self.slots.get(l.0 as usize).copied().flatten() {
                Some(Slots::Ptr(i)) => i..i + 1,
                Some(Slots::Agg(i, len)) => i..i + len,
                None => return,
            };
            st[range].fill(Lat::Varies);
        }

        pub fn step(&self, st: &mut [Lat], s: &Stmt) {
            let Stmt::Assign(d, rv) = s else { return };
            if let Some(i) = self.slot(d) {
                st[i] = self.eval(st, rv);
                return;
            }
            let whole_agg = match self.slots.get(d.local.0 as usize).copied().flatten() {
                Some(Slots::Agg(i, len)) if d.proj.is_empty() => Some((i, len)),
                _ => None,
            };
            match (whole_agg, rv) {
                (Some((i, len)), Rvalue::Aggregate(_, ops)) if ops.len() == len => {
                    let values: Vec<Lat> = ops.iter().map(|op| self.value(st, op)).collect();
                    st[i..i + len].copy_from_slice(&values);
                }
                (Some((i, len)), Rvalue::Use(Operand::Copy(src))) => {
                    match self.slots.get(src.local.0 as usize).copied().flatten() {
                        Some(Slots::Agg(j, n)) if n == len && src.proj.is_empty() => {
                            st.copy_within(j..j + len, i);
                        }
                        _ => self.clobber(st, d.local),
                    }
                }
                _ => self.clobber(st, d.local),
            }
        }

        pub fn known(&self, st: &[Lat], l: Local) -> Option<(StaticId, i128)> {
            match self.slots.get(l.0 as usize).copied().flatten()? {
                Slots::Ptr(i) => match st[i] {
                    Lat::Known(id, off) => Some((id, off)),
                    _ => None,
                },
                Slots::Agg(..) => None,
            }
        }
    }
}

/// What `slot_loads` finds with the dense analysis.
fn dense_loads(p: &Program, func: &Function) -> Vec<Load> {
    let Some(d) = dense::Dense::of(&p.aggs, &p.statics, func) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (b, block) in func.blocks.iter().enumerate() {
        let Some(mut st) = d.entry[b].clone() else {
            continue;
        };
        for i in 0..=block.stmts.len() {
            let mut check = |op: &Operand| {
                let Operand::Copy(place) = op else { return };
                let Some((id, base)) = d.known(&st, place.local) else {
                    return;
                };
                let Some(a) = slot_read(&p.aggs, base, place) else {
                    return;
                };
                let relocs = &p.statics[id.0 as usize].relocs;
                if let Some((_, target)) = relocs.iter().find(|&&(off, _)| i128::from(off) == a.lo)
                {
                    out.push(((b, i), place.clone(), target.clone()));
                }
            };
            match block.stmts.get(i) {
                Some(s) => stmt_operands(s, &mut check),
                None => term_operands(&block.term, &mut check),
            }
            if let Some(s) = block.stmts.get(i) {
                d.step(&mut st, s);
            }
        }
    }
    out
}

/// A small deterministic generator (xorshift).
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n as u64) as usize
    }
}

struct Gen<'a> {
    rng: Rng,
    env: &'a Env,
    ptrs: Vec<Local>,
    /// Aggregate locals, of every shape, with their shape and field count.
    aggs: Vec<(Local, AggId, usize)>,
    blocks: usize,
    /// Leave out what forgets a slot, so that facts survive to the loads.
    quiet: bool,
    loaded: Local,
}

impl Gen<'_> {
    fn ptr(&mut self) -> Local {
        self.ptrs[self.rng.below(self.ptrs.len())]
    }

    fn agg(&mut self) -> (Local, AggId, usize) {
        self.aggs[self.rng.below(self.aggs.len())]
    }

    /// A top-level field of an aggregate local; rarely one it does not have.
    fn agg_field(&mut self) -> Place {
        let (a, _, len) = self.agg();
        let k = if self.rng.below(20) == 0 {
            len
        } else {
            self.rng.below(len)
        };
        field(a, k as u32)
    }

    fn operand(&mut self) -> Operand {
        match self.rng.below(if self.quiet { 5 } else { 7 }) {
            0 => address(self.env.vtable),
            1 => address(self.env.other),
            2 => copy_place(self.agg_field()),
            5 => address(self.env.plain),
            6 => int(0, Ptr),
            _ => copy_local(self.ptr()),
        }
    }

    /// A place through a pointer: most of them loads of a pointer a relocation can answer.
    fn through(&mut self) -> Place {
        let p = self.ptr();
        let view = |id| Proj::Deref(Ty::Agg(id));
        let (pair, nested, triple) = (self.env.pair, self.env.nested, self.env.triple);
        let proj = match self.rng.below(10) {
            0 => vec![view(pair), Proj::Field(self.rng.below(2) as u32)],
            1 => vec![Proj::Deref(Ptr), Proj::Deref(Ptr)],
            2 => vec![Proj::Deref(I64)],
            3 => vec![view(triple), Proj::Field(self.rng.below(3) as u32)],
            4 => vec![
                view(nested),
                Proj::Field(1),
                Proj::Field(self.rng.below(2) as u32),
            ],
            5 => vec![view(nested), Proj::Field(self.rng.below(2) as u32)],
            6 => vec![view(pair), Proj::Cast(triple), Proj::Field(2)],
            _ => vec![Proj::Deref(Ptr)],
        };
        Place { local: p, proj }
    }

    /// A local only loads write.
    fn fresh_load(&self) -> Place {
        Place::local(self.loaded)
    }

    fn stmt(&mut self) -> Stmt {
        let d = Place::local(self.ptr());
        let rv = Rvalue::Use;
        match self.rng.below(if self.quiet { 9 } else { 14 }) {
            0 | 1 => Stmt::Assign(d, rv(self.operand())),
            2 | 3 => {
                let c = 8 * self.rng.below(3) as i128;
                Stmt::Assign(d, bin(BinOp::PtrAdd, self.operand(), int(c, I64)))
            }
            4 if self.quiet => Stmt::Assign(self.fresh_load(), rv(copy_place(self.through()))),
            4 | 5 => Stmt::Assign(d, rv(copy_place(self.through()))),
            6 => {
                // Rarely with the wrong number of operands.
                let (a, shape, len) = self.agg();
                let count = if self.rng.below(20) == 0 {
                    len + 1
                } else {
                    len
                };
                let fields = (0..count).map(|_| self.operand()).collect();
                Stmt::Assign(Place::local(a), Rvalue::Aggregate(shape, fields))
            }
            // Between any two aggregate locals: the same one, or another shape.
            7 => {
                let ((to, ..), (from, ..)) = (self.agg(), self.agg());
                Stmt::Assign(Place::local(to), rv(copy_local(from)))
            }
            8 => Stmt::Assign(self.agg_field(), rv(self.operand())),
            9 => Stmt::Assign(self.through(), rv(self.operand())),
            10 => Stmt::Assign(d, bin(BinOp::Add, self.operand(), int(8, I64))),
            11 => {
                let dst = copy_place(self.through());
                Stmt::MemCopy {
                    dst,
                    src: copy_local(self.ptr()),
                    size: 8,
                }
            }
            12 => {
                let mut p = self.agg_field();
                let inner = [Proj::Deref(Ptr), Proj::Field(0)][self.rng.below(2)].clone();
                p.proj.push(inner);
                Stmt::Assign(p, rv(self.operand()))
            }
            _ => Stmt::Nop,
        }
    }

    fn term(&mut self) -> Terminator {
        let next = |g: &mut Self| BlockId(g.rng.below(g.blocks) as u32);
        match self.rng.below(8) {
            0 => Terminator::Return(self.operand()),
            1 | 2 => Terminator::Goto(next(self)),
            3 | 4 => Terminator::Branch {
                cond: copy_place(self.through()),
                then: next(self),
                els: next(self),
            },
            5 => Terminator::Switch {
                value: copy_local(self.ptr()),
                cases: vec![(0, next(self)), (1, next(self))],
                default: next(self),
            },
            _ => Terminator::Call {
                callee: Callee::Extern(self.env.keep),
                args: vec![copy_place(self.through())],
                dest: (!self.quiet && self.rng.below(2) == 0).then(|| {
                    if self.rng.below(2) == 0 {
                        Place::local(self.ptr())
                    } else {
                        self.agg_field()
                    }
                }),
                next: next(self),
            },
        }
    }
}

fn random_function(env: &Env, seed: u64) -> Function {
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let blocks = 1 + rng.below(12);
    let mut fb = FuncBuilder::export("main", &[Ptr], Unit);
    // The parameter and a local whose address some function takes are never known.
    let mut ptrs = vec![fb.param(0)];
    ptrs.extend((0..1 + rng.below(8)).map(|_| fb.local(Ptr)));
    let shapes = [(env.pair, 2), (env.nested, 2), (env.triple, 3)];
    let aggs: Vec<(Local, AggId, usize)> = (0..1 + rng.below(4))
        .map(|_| {
            let (shape, len) = shapes[rng.below(shapes.len())];
            (fb.local(Ty::Agg(shape)), shape, len)
        })
        .collect();
    let ids: Vec<BlockId> = (0..blocks).map(|_| fb.block()).collect();
    let loaded = fb.local(Ptr);
    let mut gen = Gen {
        rng,
        env,
        ptrs,
        aggs,
        blocks,
        quiet: seed.is_multiple_of(2),
        loaded,
    };
    for &b in &ids {
        for _ in 0..gen.rng.below(7) {
            let s = gen.stmt();
            fb.push(b, s);
        }
        if gen.rng.below(30) == 0 {
            let (d, p) = (gen.ptr(), gen.ptr());
            fb.assign(b, d, Rvalue::AddrOf(Place::local(p)));
        }
        let t = gen.term();
        fb.term(b, t);
    }
    fb.finish()
}

#[test]
fn random_functions_fold_what_tracking_every_slot_folds() {
    let env = env();
    let p = Program::clone(&env.pb.p);
    let (mut folded, mut tracked_less, mut analyzed) = (0, 0, 0);
    for seed in 1..=20000 {
        let func = random_function(&env, seed);
        let expected = dense_loads(&p, &func);
        let got = slot_loads(&p.aggs, &p.statics, &func);
        assert_eq!(got, expected, "seed {seed}\n{func:?}");
        folded += got.len();
        if let Some(d) = dense::Dense::of(&p.aggs, &p.statics, &func) {
            analyzed += 1;
            let all = d.entry[0].as_ref().map_or(0, Vec::len);
            let kept = Addrs::of(&p.aggs, &p.statics, &func)
                .map_or(0, |a| a.tracked.iter().flatten().count());
            tracked_less += usize::from(kept < all);
        }
    }
    // The generator has to reach what this test is about.
    assert!(folded > 1000, "{folded} loads folded");
    assert!(
        tracked_less > analyzed / 4,
        "{tracked_less} of {analyzed} functions track fewer slots"
    );
}
