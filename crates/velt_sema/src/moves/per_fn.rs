//! One function's move check ([`check_fn`]), apart from the `Ctx`: `check_all` gathers what it
//! reads on the checking thread and runs it for every function on several (`crate::par`).

use rustc_hash::{FxHashMap, FxHashSet};

use velt_common::{Diagnostic, Span};

use super::state::State;
use super::{creates_escaping, Moves, Writers};
use crate::hir::{Capture, DefId, LocalId, TyId};

/// What [`check_fn`] reads of one function, gathered from the `Ctx` on the checking thread.
pub(super) struct FnInput<'a> {
    pub(super) def: DefId,
    pub(super) f: &'a crate::hir::FnDef,
    pub(super) soft: FxHashSet<Span>,
    pub(super) using: Vec<bool>,
    pub(super) held: FxHashMap<LocalId, DefId>,
    pub(super) late: Vec<bool>,
}

/// The tables of the whole program every function's check reads.
pub(super) struct Tables<'a> {
    pub(super) captures: &'a FxHashMap<DefId, Vec<Capture>>,
    pub(super) escaping: &'a FxHashSet<DefId>,
    pub(super) writers: &'a Writers,
    pub(super) own_writers: &'a Writers,
    pub(super) kept: &'a FxHashMap<DefId, FxHashSet<LocalId>>,
    pub(super) generators: &'a FxHashMap<DefId, bool>,
    pub(super) nested_only: &'a FxHashSet<LocalId>,
    pub(super) per_iteration: &'a FxHashSet<(DefId, LocalId)>,
    pub(super) async_captured: &'a FxHashSet<(DefId, LocalId)>,
    pub(super) shared: &'a [Vec<bool>],
    pub(super) copy: &'a [Vec<bool>],
    pub(super) promise: &'a [Vec<bool>],
    pub(super) never: TyId,
}

/// What one function's check found.
pub(super) struct FnChecked {
    pub(super) errors: Vec<Diagnostic>,
    pub(super) nested: Vec<(String, Span)>,
    pub(super) reused: FxHashSet<(Span, LocalId)>,
    pub(super) boxed: FxHashSet<LocalId>,
    /// `for (let …)` variables that cannot be one cell (`crate::ownership::unshareable_nested`).
    pub(super) unshared: Vec<LocalId>,
}

/// The move check of one function: no `Ctx`, so it may run on any thread (`crate::par`).
pub(super) fn check_fn(t: &Tables, input: &FnInput) -> FnChecked {
    let (def, f) = (input.def, input.f);
    let i = def.0 as usize;
    let mut m = Moves {
        locals: &f.body.locals,
        using: input.using.clone(),
        captures: t.captures,
        escaping: t.escaping,
        held: input.held.clone(),
        in_step: false,
        holder: None,
        open: vec![],
        never: t.never,
        boxable: t.shared[i]
            .iter()
            .zip(&t.copy[i])
            .enumerate()
            .map(|(l, (s, c))| (*s || *c) && !t.async_captured.contains(&(def, LocalId(l as u32))))
            .collect(),
        threaded: (0..f.body.locals.len())
            .map(|l| t.async_captured.contains(&(def, LocalId(l as u32))))
            .collect(),
        nested: vec![],
        shared: t.shared[i].clone(),
        promise: t.promise[i].clone(),
        writers: t.writers,
        kept: t.kept,
        generators: t.generators,
        boxed: FxHashSet::default(),
        own_captures: match f.is_async && !f.is_generator && !f.shares_captures {
            true => FxHashSet::default(),
            false => f.captures.iter().map(|c| c.inner).collect(),
        },
        tries: vec![],
        late: input.late.clone(),
        capturing: false,
        late_reported: FxHashSet::default(),
        report: false,
        errors: vec![],
        loops: vec![],
        soft: input.soft.clone(),
        reused: FxHashSet::default(),
    };
    // A silent pass first (only bodies creating an escaping closure can need a cell): a
    // variable found to need a shared cell late in the body is no move error earlier either.
    if creates_escaping(&f.body.block, t.escaping) {
        m.report = false;
        m.block(&f.body.block, &mut Some(State::new(f.body.locals.len())));
    }
    m.report = true;
    m.block(&f.body.block, &mut Some(State::new(f.body.locals.len())));
    // A `for (let …)` variable cannot be one cell (each iteration has its own, copied into
    // the next one): when it needs one only because a closure created inside an escaping
    // closure assigns it, and not anyway, it is an error.
    let mut cannot: Vec<LocalId> = m
        .boxed
        .iter()
        .copied()
        .filter(|l| t.nested_only.contains(l) && t.per_iteration.contains(&(def, *l)))
        .collect();
    // In declaration order, not the hash set's: the errors come in that order.
    cannot.sort_unstable();
    let mut unshared = vec![];
    if !cannot.is_empty() {
        let (boxed, errors, reused) = (m.boxed.clone(), m.errors.len(), m.reused.clone());
        m.writers = t.own_writers;
        m.report = false;
        m.boxed.clear();
        m.block(&f.body.block, &mut Some(State::new(f.body.locals.len())));
        m.block(&f.body.block, &mut Some(State::new(f.body.locals.len())));
        let anyway = std::mem::replace(&mut m.boxed, boxed);
        m.errors.truncate(errors);
        m.reused = reused;
        m.writers = t.writers;
        for l in cannot.into_iter().filter(|l| !anyway.contains(l)) {
            m.boxed.remove(&l);
            unshared.push(l);
        }
    }
    FnChecked {
        errors: m.errors,
        nested: m.nested,
        reused: m.reused,
        boxed: m.boxed,
        unshared,
    }
}
