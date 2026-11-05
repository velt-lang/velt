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
    slot_loads(&p.aggs, &p.statics, &p.funcs[f.0 as usize]).len()
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
    pairs: Vec<Local>,
    blocks: usize,
    /// Leave out what forgets a slot, so that facts survive to the loads.
    quiet: bool,
    loaded: Local,
}

impl Gen<'_> {
    fn ptr(&mut self) -> Local {
        self.ptrs[self.rng.below(self.ptrs.len())]
    }

    fn pair_field(&mut self) -> Place {
        let a = self.pairs[self.rng.below(self.pairs.len())];
        field(a, self.rng.below(2) as u32)
    }

    fn operand(&mut self) -> Operand {
        match self.rng.below(if self.quiet { 5 } else { 7 }) {
            0 => address(self.env.vtable),
            1 => address(self.env.other),
            2 => copy_place(self.pair_field()),
            5 => address(self.env.plain),
            6 => int(0, Ptr),
            _ => copy_local(self.ptr()),
        }
    }

    /// A place through a pointer: most of them loads of a pointer a relocation can answer.
    fn through(&mut self) -> Place {
        let p = self.ptr();
        let pair = Ty::Agg(self.env.pair);
        let proj = match self.rng.below(6) {
            0 => vec![Proj::Deref(pair), Proj::Field(self.rng.below(2) as u32)],
            1 => vec![Proj::Deref(Ptr), Proj::Deref(Ptr)],
            2 => vec![Proj::Deref(I64)],
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
        let (a, b) = (self.pairs[0], self.pairs[self.rng.below(self.pairs.len())]);
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
                let fields = vec![self.operand(), self.operand()];
                Stmt::Assign(Place::local(b), Rvalue::Aggregate(self.env.pair, fields))
            }
            7 => Stmt::Assign(Place::local(a), rv(copy_local(b))),
            8 => Stmt::Assign(self.pair_field(), rv(self.operand())),
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
                let mut p = self.pair_field();
                p.proj.push(Proj::Deref(Ptr));
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
                        self.pair_field()
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
    let pairs: Vec<Local> = (0..1 + rng.below(3))
        .map(|_| fb.local(Ty::Agg(env.pair)))
        .collect();
    let ids: Vec<BlockId> = (0..blocks).map(|_| fb.block()).collect();
    let loaded = fb.local(Ptr);
    let mut gen = Gen {
        rng,
        env,
        ptrs,
        pairs,
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
