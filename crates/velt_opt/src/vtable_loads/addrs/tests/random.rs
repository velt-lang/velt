//! Random functions, compared with the analysis before it was sparse.

use super::super::super::{slot_read, Load};
use super::*;
use velt_vir::vir::Program;

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
pub(super) fn dense_loads(p: &Program, func: &Function) -> Vec<Load> {
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
