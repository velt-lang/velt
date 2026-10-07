//! Per-function summaries for the `with` checks (super module docs): which parameters a
//! function may store a part of into which other parameters (`addTo(xs) { xs.push(this.inner) }`
//! stores from `this` into `xs`), and from which parameters it may make a promise (`fire(o) {
//! const p = bump(o); }`). A call in a callback then crosses the lock only where its callee's
//! summary says so.
//!
//! A summary is a fixpoint over the program's functions (a callee's summary feeds its callers').
//! Inside a body each local reaches a set of the function's parameters, like the regions of a
//! callback (`super::regions`): bound to an expression, it reaches what the expression mentions;
//! a fresh local reaches what is stored into it. A store into a place reaching parameter `j` of
//! a value reaching parameter `i` is a flow `i → j`. Calls follow their callee's summary; a call
//! whose callee is not known (a function value, a virtual or interface method, an extern, an
//! intrinsic) may store any argument into any argument it may modify, and a function value it
//! calls may keep its arguments. Copy values and strings never flow.

use std::collections::{BTreeSet, HashMap, HashSet};

use velt_common::Span;

use super::regions::{pat_locals, reach};
use crate::body::places::{is_place, place_root};
use crate::ctx::Ctx;
use crate::defs::{BodyState, FnKind};
use crate::hir::{
    Callee, Def, DefId, Expr, ExprKind as E, FnDef, Intrinsic, LocalId, Pat, Stmt, StmtKind as S,
    TyId, UseMode,
};
use crate::visit::{self, VisitMut};

/// What a call of a function may do with its arguments (indices of `FnDef::params`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Summary {
    /// `(i, j)`: a part of argument `i` may be stored into argument `j`.
    pub(super) flows: BTreeSet<(usize, usize)>,
    /// Arguments a promise the call makes (and may leave running) may use.
    pub(super) promises: u64,
}

/// The summaries of the program's functions (not of closures, which are called as values).
pub(super) struct Summaries {
    sums: HashMap<DefId, Summary>,
    /// The variables each closure of the program captures (in the body making it).
    captures: HashMap<DefId, Vec<LocalId>>,
}

impl Summaries {
    pub(super) fn compute(cx: &mut Ctx) -> Self {
        let fns: Vec<DefId> = cx
            .fn_defs
            .iter()
            .copied()
            .filter(|d| {
                let info = cx.fn_info(*d);
                info.state == BodyState::Done && info.kind != FnKind::Closure
            })
            .collect();
        let captures = (0..cx.defs.len())
            .filter_map(|i| match &cx.defs[i] {
                Some(Def::Fn(f)) if !f.captures.is_empty() => Some((
                    DefId(i as u32),
                    f.captures.iter().map(|k| k.outer).collect(),
                )),
                _ => None,
            })
            .collect();
        let mut s = Summaries {
            sums: HashMap::new(),
            captures,
        };
        // Summaries only grow, and are bounded by the parameter pairs: this ends.
        loop {
            let mut changed = false;
            for &d in &fns {
                let Some(Def::Fn(mut f)) = cx.defs[d.0 as usize].take() else {
                    continue;
                };
                let new = summarize(cx, &s, &mut f);
                cx.defs[d.0 as usize] = Some(Def::Fn(f));
                if s.sums.get(&d) != Some(&new) {
                    s.sums.insert(d, new);
                    changed = true;
                }
            }
            if !changed {
                return s;
            }
        }
    }

    pub(super) fn get(&self, d: DefId) -> Option<&Summary> {
        self.sums.get(&d)
    }

    /// The variables closure `n` captures.
    pub(super) fn captured(&self, n: DefId) -> Vec<LocalId> {
        self.captures.get(&n).cloned().unwrap_or_default()
    }
}

/// The parameter bit of the parameter at `i` (parameters past 63 share the last bit).
fn bit(i: usize) -> u64 {
    1 << i.min(63)
}

fn bits_of(b: u64) -> impl Iterator<Item = usize> {
    (0..64).filter(move |i| b & (1 << i) != 0)
}

/// Summary of body `f` given its callees' current summaries.
fn summarize(cx: &mut Ctx, s: &Summaries, f: &mut FnDef) -> Summary {
    let mut w = Walk {
        cx,
        s,
        bits: HashMap::new(),
        out: Summary::default(),
        changed: false,
        spawned: HashSet::new(),
    };
    for (i, p) in f.params.iter().enumerate() {
        w.bits.insert(p.local, bit(i));
    }
    loop {
        w.changed = false;
        visit::block(&mut f.body.block, &mut w);
        if !w.changed {
            return w.out;
        }
    }
}

struct Walk<'a, 's, 'm> {
    cx: &'a mut Ctx<'m>,
    s: &'s Summaries,
    /// The parameters each local may reach.
    bits: HashMap<LocalId, u64>,
    out: Summary,
    changed: bool,
    /// Spans of the calls `spawn` takes.
    spawned: HashSet<Span>,
}

impl Walk<'_, '_, '_> {
    fn mentions(&mut self, e: &Expr) -> u64 {
        let bits = &self.bits;
        let local = |l: LocalId| bits.get(&l).copied().unwrap_or(0);
        let s = self.s;
        reach(self.cx, e, &local, &|n| s.captured(n))
    }

    fn add(&mut self, l: LocalId, b: u64) {
        let e = self.bits.entry(l).or_insert(0);
        if *e | b != *e {
            *e |= b;
            self.changed = true;
        }
    }

    /// A value reaching `b` stored into `place`.
    fn store(&mut self, place: &Expr, b: u64) {
        if b == 0 {
            return;
        }
        let root = place_root(place);
        let dest = match root {
            Some(l) => self.bits.get(&l).copied().unwrap_or(0),
            None => self.mentions(place),
        };
        if dest == 0 {
            if let Some(l) = root {
                self.add(l, b);
            }
            return;
        }
        for i in bits_of(b) {
            for j in bits_of(dest) {
                if i != j && self.out.flows.insert((i, j)) {
                    self.changed = true;
                }
            }
        }
    }

    fn bind(&mut self, p: &Pat, b: u64) {
        let mut ls = vec![];
        pat_locals(p, &mut ls);
        for l in ls {
            self.add(l, b);
        }
    }

    fn promise(&mut self, b: u64) {
        if self.out.promises | b != self.out.promises {
            self.out.promises |= b;
            self.changed = true;
        }
    }

    fn call(&mut self, ty: TyId, span: Span, callee: &Callee, args: &[Expr]) {
        let bits: Vec<u64> = args.iter().map(|a| self.mentions(a)).collect();
        let fn_bits = match callee {
            Callee::Indirect(c) => self.mentions(c),
            _ => 0,
        };
        let skip = matches!(
            callee,
            Callee::Intrinsic(
                Intrinsic::Transfer
                    | Intrinsic::Share
                    | Intrinsic::Clone
                    | Intrinsic::Spawn
                    | Intrinsic::PromiseWiden
                    | Intrinsic::MutexWith
                    | Intrinsic::NeedsTransfer
                    | Intrinsic::NeedsDrop
            )
        );
        if let Callee::Intrinsic(Intrinsic::Spawn) = callee {
            // `spawn` transfers what it is given: the spawned call keeps nothing.
            self.spawned.extend(args.iter().map(|a| a.span));
        }
        if skip {
            return;
        }
        let spawned = self.spawned.contains(&span);
        if self.cx.holds_promise(ty) && !spawned {
            let all = bits.iter().fold(fn_bits, |b, x| b | x);
            self.promise(all);
        }
        for (a, b) in call_flows(self.s, callee, args) {
            match b {
                Some(b) => self.store(&args[b], bits[a]),
                None => self.store_into_fn(fn_bits, bits[a]),
            }
        }
        if let (Callee::Def(g, _), false) = (callee, spawned) {
            let p = self.s.get(*g).map_or(0, |s| s.promises);
            let used = bits_of(p).filter_map(|a| bits.get(a)).fold(0, |b, x| b | x);
            self.promise(used);
        }
    }

    /// A function value reaching `dest` given a value reaching `b`: it may keep it in what it
    /// captured.
    fn store_into_fn(&mut self, dest: u64, b: u64) {
        for i in bits_of(b) {
            for j in bits_of(dest) {
                if i != j && self.out.flows.insert((i, j)) {
                    self.changed = true;
                }
            }
        }
    }
}

/// The flows of a call: `(argument, destination)` pairs, the destination an argument index or
/// `None` for the function value called. A known callee follows its summary; any other may
/// store any argument into any argument it modifies, or keep it in the function value.
pub(super) fn call_flows(
    s: &Summaries,
    callee: &Callee,
    args: &[Expr],
) -> Vec<(usize, Option<usize>)> {
    if let Callee::Def(g, _) = callee {
        if let Some(sum) = s.get(*g) {
            return sum
                .flows
                .iter()
                .filter(|(a, b)| *a < args.len() && *b < args.len())
                .map(|&(a, b)| (a, Some(b)))
                .collect();
        }
    }
    let mut out = vec![];
    for (b, d) in args.iter().enumerate() {
        if is_place(d) && outer_mode(d) == Some(UseMode::BorrowMut) {
            out.extend((0..args.len()).filter(|a| *a != b).map(|a| (a, Some(b))));
        }
    }
    if matches!(callee, Callee::Indirect(_)) {
        out.extend((0..args.len()).map(|a| (a, None)));
    }
    out
}

/// The use mode of a place's outermost node.
pub(super) fn outer_mode(e: &Expr) -> Option<UseMode> {
    match &e.kind {
        E::Local(_, m)
        | E::Field { mode: m, .. }
        | E::Index { mode: m, .. }
        | E::UnwrapSome(_, m)
        | E::UnwrapVariant { mode: m, .. } => Some(*m),
        E::Downcast(x) => outer_mode(x),
        _ => None,
    }
}

impl VisitMut for Walk<'_, '_, '_> {
    fn stmt(&mut self, st: &mut Stmt) {
        match &st.kind {
            S::Let {
                local,
                init: Some(init),
            } => {
                let b = self.mentions(init);
                self.add(*local, b);
            }
            S::LetPat { pat, init } => {
                let b = self.mentions(init);
                self.bind(pat, b);
            }
            S::ForOf { binding, iter, .. } => {
                let b = self.mentions(iter);
                self.bind(binding, b);
            }
            _ => {}
        }
    }

    fn expr(&mut self, e: &mut Expr) {
        match &e.kind {
            E::Assign { place, value } => {
                let b = self.mentions(value);
                match place.kind {
                    // Rebinding a local only changes what it reaches.
                    E::Local(l, _) => self.add(l, b),
                    _ => {
                        let place = (**place).clone();
                        self.store(&place, b);
                    }
                }
            }
            E::Call { .. } => {
                let (ty, span) = (e.ty, e.span);
                if let E::Call { callee, args } = &mut e.kind {
                    let taken = std::mem::take(args);
                    let callee = callee.clone();
                    self.call(ty, span, &callee, &taken);
                    if let E::Call { args, .. } = &mut e.kind {
                        *args = taken;
                    }
                }
            }
            E::Match { scrutinee, arms } => {
                let b = self.mentions(scrutinee);
                let pats: Vec<Pat> = arms.iter().map(|a| a.pat.clone()).collect();
                for p in &pats {
                    self.bind(p, b);
                }
            }
            _ => {}
        }
    }
}
