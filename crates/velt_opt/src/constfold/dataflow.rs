//! Sparse-conditional-style constant propagation over register-like locals: a forward
//! dataflow on the lattice `Undef < Known(v) < Varying` that only follows CFG edges which can
//! actually be taken given the constants known so far (so code behind a constant-false branch
//! does not pollute the analysis).

use velt_vir::vir::{Function, Local, Operand, Rvalue, Stmt, Terminator, Ty};

use super::liveness::Liveness;
use super::value::{self, Value};
use crate::locals::{as_register, Usage};

/// Lattice element for one local.
#[derive(Clone, Copy, Debug)]
pub(super) enum Lat {
    /// No assignment seen yet on any path (optimistic top).
    Undef,
    /// Always this value.
    Known(Value),
    /// Unknown at compile time.
    Varying,
}

impl Lat {
    /// Meet `other` into `self`; returns whether `self` changed.
    fn meet(&mut self, other: Lat) -> bool {
        let new = match (*self, other) {
            (Lat::Varying, _) | (_, Lat::Undef) => return false,
            (Lat::Undef, x) => x,
            (Lat::Known(a), Lat::Known(b)) if a.same(&b) => return false,
            _ => Lat::Varying,
        };
        *self = new;
        true
    }
}

/// Lattice values of the tracked locals at one program point (indexed by `Facts::slot`).
pub(super) type State = Vec<Lat>;

/// Above this many (block × live tracked local) cells the global analysis is skipped and each
/// block is folded on its own (bounded memory on huge post-inlining functions).
const MAX_CELLS: usize = 4_000_000;

/// Per-function context: which locals are tracked and their types.
pub(super) struct Facts {
    /// Local usage (decides which locals are register-like).
    pub usage: Usage,
    /// Local types.
    pub tys: Vec<Ty>,
    /// Dense state index of each register-like local.
    slots: Vec<Option<usize>>,
    /// Number of tracked locals.
    tracked: usize,
    /// Number of tracked params: slots `0..param_slots` (they start `Varying`).
    param_slots: usize,
}

impl Facts {
    /// Facts for `func`.
    pub fn of(func: &Function) -> Facts {
        let usage = Usage::of(func);
        let mut tracked = 0;
        let slots: Vec<Option<usize>> = (0..func.locals.len())
            .map(|i| {
                usage.is_register(Local(i as u32)).then(|| {
                    tracked += 1;
                    tracked - 1
                })
            })
            .collect();
        let param_slots = slots.iter().take(func.params.len()).flatten().count();
        Facts {
            usage,
            tys: func.locals.iter().map(|l| l.ty).collect(),
            slots,
            tracked,
            param_slots,
        }
    }

    /// Number of tracked locals (the length of a [`State`]).
    pub fn tracked(&self) -> usize {
        self.tracked
    }

    /// State slot of a place that is exactly a tracked local.
    pub fn slot(&self, p: &velt_vir::vir::Place) -> Option<usize> {
        as_register(&self.usage, p).and_then(|l| self.slots[l.0 as usize])
    }

    /// State where nothing is known: the scratch state blocks are evaluated in (only the
    /// slots live at a block's start are loaded from its entry facts, see [`load`]).
    pub fn unknown(&self) -> State {
        vec![Lat::Varying; self.tracked]
    }

    /// Known value of an operand, if any.
    pub fn value(&self, st: &State, op: &Operand) -> Option<Value> {
        match op {
            Operand::Const(c, ty) => Value::from_const(c, *ty),
            Operand::Copy(p) => match self.slot(p).map(|i| st[i]) {
                Some(Lat::Known(v)) => Some(v),
                _ => None,
            },
        }
    }

    /// Type of an operand whose value is known (constants and register locals).
    pub fn ty(&self, op: &Operand) -> Ty {
        self.known_ty(op)
            .expect("ICE: typed operand is a constant or a whole local")
    }

    /// Type of a constant or whole-local operand; `None` for projected places (their type
    /// needs aggregate layouts, and no folding decision depends on them).
    pub fn known_ty(&self, op: &Operand) -> Option<Ty> {
        match op {
            Operand::Const(_, ty) => Some(*ty),
            Operand::Copy(p) if p.proj.is_empty() => Some(self.tys[p.local.0 as usize]),
            Operand::Copy(_) => None,
        }
    }

    /// Evaluate an rvalue in state `st`.
    pub fn eval(&self, st: &State, rv: &Rvalue) -> Option<Value> {
        match rv {
            Rvalue::Use(a) => self.value(st, a),
            Rvalue::Unary(op, a) => value::unary(*op, self.value(st, a)?, self.ty(a)),
            Rvalue::Binary(op, a, b) => {
                let (x, y) = (self.value(st, a)?, self.value(st, b)?);
                value::binary(*op, x, y, self.ty(a))
            }
            Rvalue::Cast(a, to) => value::cast(self.value(st, a)?, self.ty(a), *to),
            Rvalue::AddrOf(_) | Rvalue::Aggregate(..) => None,
        }
    }

    /// Apply a statement to the state.
    pub fn transfer(&self, st: &mut State, s: &Stmt) {
        if let Stmt::Assign(place, rv) = s {
            if let Some(i) = self.slot(place) {
                st[i] = self.eval(st, rv).map_or(Lat::Varying, Lat::Known);
            }
        }
    }

    /// Apply the effect of a terminator on its fall-through state (call results).
    pub fn transfer_term(&self, st: &mut State, t: &Terminator) {
        if let Terminator::Call { dest: Some(d), .. } = t {
            if let Some(i) = self.slot(d) {
                st[i] = Lat::Varying;
            }
        }
    }

    /// Successors that can be taken given `st`.
    pub fn feasible(&self, st: &State, t: &Terminator) -> Vec<velt_vir::vir::BlockId> {
        match t {
            Terminator::Branch { cond, then, els } => match self.value(st, cond) {
                Some(Value::Int(c)) => vec![if c != 0 { *then } else { *els }],
                _ => vec![*then, *els],
            },
            Terminator::Switch {
                value,
                cases,
                default,
            } => match self.value(st, value) {
                Some(Value::Int(v)) => vec![switch_target(v, self.ty(value), cases, *default)],
                _ => crate::visit::successors(t),
            },
            _ => crate::visit::successors(t),
        }
    }
}

/// The block a switch on the known value `v` of type `ty` jumps to. Case keys are compared
/// by bit pattern at the value's width, like the backend does.
pub(super) fn switch_target(
    v: i128,
    ty: Ty,
    cases: &[(i128, velt_vir::vir::BlockId)],
    default: velt_vir::vir::BlockId,
) -> velt_vir::vir::BlockId {
    let key = value::normalize(v, ty);
    cases
        .iter()
        .find(|(k, _)| value::normalize(*k, ty) == key)
        .map_or(default, |(_, b)| *b)
}

/// Entry facts of one block: the lattice values of its live-in slots, in the order of
/// `Liveness::live_in`.
pub(super) type Entry = Vec<Lat>;

/// Load a block's entry facts into the scratch state.
pub(super) fn load(st: &mut State, live_in: &[usize], entry: &Entry) {
    for (&slot, &lat) in live_in.iter().zip(entry) {
        st[slot] = lat;
    }
}

/// Return the slots a block touched (its live-in and assigned slots) to `Varying`.
pub(super) fn reset(st: &mut State, liveness: &Liveness, b: usize) {
    for &slot in liveness.live_in[b].iter().chain(&liveness.defs[b]) {
        st[slot] = Lat::Varying;
    }
}

/// Solve the dataflow; returns the entry facts of each reachable block (`None` =
/// unreachable), or `None` overall when the function is too big to analyze globally. States
/// only hold the slots live at each block's start (liveness.rs), so long straight-line
/// functions cost time proportional to their size, not size × locals.
pub(super) fn solve(func: &Function, facts: &Facts) -> Option<(Liveness, Vec<Option<Entry>>)> {
    let liveness = Liveness::of(func, facts, MAX_CELLS)?;
    let init: Entry = liveness.live_in[0]
        .iter()
        .map(|&slot| match slot < facts.param_slots {
            true => Lat::Varying,
            false => Lat::Undef,
        })
        .collect();
    let mut entry: Vec<Option<Entry>> = vec![None; func.blocks.len()];
    entry[0] = Some(init);
    let mut work = vec![0usize];
    let mut queued = vec![false; func.blocks.len()];
    queued[0] = true;
    let mut st = facts.unknown();
    while let Some(b) = work.pop() {
        queued[b] = false;
        let e = entry[b].as_ref().expect("ICE: queued block has a state");
        load(&mut st, &liveness.live_in[b], e);
        let block = &func.blocks[b];
        for s in &block.stmts {
            facts.transfer(&mut st, s);
        }
        let targets = facts.feasible(&st, &block.term);
        facts.transfer_term(&mut st, &block.term);
        for t in targets {
            let t = t.0 as usize;
            let live = &liveness.live_in[t];
            let changed = match &mut entry[t] {
                None => {
                    entry[t] = Some(live.iter().map(|&slot| st[slot]).collect());
                    true
                }
                Some(e) => e
                    .iter_mut()
                    .zip(live)
                    .fold(false, |c, (x, &slot)| x.meet(st[slot]) | c),
            };
            if changed && !queued[t] {
                queued[t] = true;
                work.push(t);
            }
        }
        reset(&mut st, &liveness, b);
    }
    Some((liveness, entry))
}
