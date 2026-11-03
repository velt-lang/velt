//! Stores by key, for [`super::fed`] (#854; docs/internals/design/write-effects.md, step 1): a
//! value put into an object through an `any` (`const self: any = this; self.out = v`, `o[k] =
//! v`) lands in a field the program never names at the store. Each function a crossing reaches
//! gets a summary of the stores by key it makes into what its parameters hold (`stores v in
//! o.*`, or `in o.out` for a constant key):
//!
//! - a store through an `any` that aliases a parameter is the function's own;
//! - a call applies the callee's summary to its arguments: a direct call, a constructor (with
//!   the new object as `this`), and an interface, virtual or bound-method call joined over the
//!   methods it may run (`super::targets`);
//! - a closure's stores into what it captured count where it is made, with its captures bound
//!   to what the maker's locals are;
//! - a call through a function-typed parameter is latent: each caller resolves it against the
//!   function it passes (`viaCb(this, setK, …)` runs `setK`'s stores); a function value read
//!   from the heap runs any of the program's function values that accept the arguments
//!   ([`super::targets::value_targets`]).
//!
//! Summaries are computed on demand from the crossings, each body prepared once (what each local
//! may be, its stores and calls), by a worklist that applies again only the events whose
//! callee's summary grew, and only what grew ([`apply`]). A call through a function value read
//! from the heap applies one join per function type. A store of a value that is not new into a value of a class (or of one of its subclasses,
//! a new one included) feeds that class's fields of the key's name, or all of them; a target of
//! an open type (`any`, `object`, a type parameter) whose object is not known feeds the fields of
//! every type the program converts to a dynamic value and of the types those hold, and an
//! interface value's those of its implementations. Package bodies are followed like the
//! program's; std's are not: its stores (`Reflect.set`) come with its declared effects (step 2).

use std::collections::VecDeque;

use rustc_hash::{FxHashMap, FxHashSet};

use velt_common::Span;

use crate::ctx::Ctx;
use crate::hir::{Def, DefId, TyId, TyKind};

use super::changes::Memo;
use super::fed::{adt_of, declaring, Field};
use super::targets::Literals;

mod apply;
mod prepare;

use apply::{summarize, Joins, Node, Pending, Shape, Shapes, Summary};
use prepare::{may_store, prepare};

/// The key of a store: a constant name, or any field.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
enum Key {
    Name(String),
    Any,
}

/// What a value in a body may be, as far as stores by key care: parameter `i` of the function
/// (an index into `FnDef::params`, captures first), a value that holds nothing (a number, a
/// literal), a new object of a type (`new C()`, `{ … }`: not new when stored, a value of its
/// type when stored into), a value of a type whose object is not followed further, one of the
/// program's functions (a function value), or a value that is not new and is only ever stored
/// (a callee's own).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
enum Arg {
    Param(u32),
    Own,
    New(TyId),
    Ty(TyId),
    Fns(Vec<DefId>),
    Other,
}

/// Where a stored value comes from: a parameter (new or not is known at the caller), or another
/// value that is not new.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Src {
    Param(u32),
    Other,
}

/// Where a field was first fed by key: the store or the call that put a value that is not new
/// in it, and how it reads (`` `self.out = …` in `ThroughAny.fill` ``).
#[derive(Clone, Debug)]
pub(super) struct Site {
    pub span: Span,
    pub text: String,
}

/// One thing a body does that stores by key: a store through an `any`, a call (the functions it
/// may run, each with the shape of the arguments it takes), a call through a function value (the
/// shape of its arguments), or making a closure (the shape of its captures).
enum Ev {
    Store {
        target: Vec<Arg>,
        key: Key,
        value: Vec<Arg>,
        site: Site,
    },
    Call {
        targets: Vec<(DefId, Shape)>,
        site: Site,
    },
    Value {
        fns: Vec<Arg>,
        actual: Shape,
        site: Site,
    },
    Closure {
        k: DefId,
        caps: Shape,
        site: Site,
    },
}

impl Ev {
    fn site(&self) -> &Site {
        match self {
            Ev::Store { site, .. }
            | Ev::Call { site, .. }
            | Ev::Value { site, .. }
            | Ev::Closure { site, .. } => site,
        }
    }
}

/// A body prepared once: its parameters' types and its events.
struct Prepared {
    params: Vec<TyId>,
    events: Vec<Ev>,
}

/// The state of one run.
struct Run<'a> {
    lits: &'a Literals,
    /// The functions whose summary may not be empty ([`may_store`]); the others are not prepared.
    may: FxHashSet<DefId>,
    prepared: FxHashMap<DefId, Prepared>,
    /// The functions prepared (or not followed), each once.
    known: FxHashSet<DefId>,
    sums: FxHashMap<Node, Summary>,
    /// For each summary, where it was applied (the function, the shapes of the arguments and
    /// the captures, and the event), applied again when it grows.
    users: FxHashMap<Node, FxHashSet<(DefId, Shape, Shape, u32)>>,
    /// What each queued function has to apply (again).
    dirty: FxHashMap<DefId, Pending>,
    work: VecDeque<DefId>,
    queued: FxHashSet<DefId>,
    /// Captures in front of each function's parameters.
    ncap: FxHashMap<DefId, u32>,
    shapes: Shapes,
    joins: Joins,
    /// How much of each summary each function applied to each shape of arguments (and of
    /// captures, for a closure being made): (stores, latent calls).
    applied: FxHashMap<(DefId, Node, Shape, Shape), (u32, u32)>,
    /// What was fed already, per type and key.
    done: FxHashSet<(TyId, Key)>,
    /// Each class fed so far, with its subclasses.
    below: FxHashMap<DefId, Vec<DefId>>,
    /// Each field fed, with where (and whether that store's target was known, not open).
    fed: FxHashMap<Field, (Site, bool)>,
    memo: Memo,
}

/// The fields stores by key may put a value that is not new in, from the functions the
/// crossings `roots` reach, with where each was first fed (module docs).
pub(super) fn keyed_fields(
    cx: &mut Ctx,
    lits: &Literals,
    roots: &[DefId],
) -> FxHashMap<Field, Site> {
    // Stores by key are where summaries come from; a program without one has nothing to feed.
    if !lits.stores_by_key() {
        return FxHashMap::default();
    }
    let may = may_store(cx, lits);
    if may.is_empty() {
        return FxHashMap::default();
    }
    let mut run = Run {
        lits,
        may,
        prepared: FxHashMap::default(),
        known: FxHashSet::default(),
        sums: FxHashMap::default(),
        users: FxHashMap::default(),
        dirty: FxHashMap::default(),
        work: VecDeque::new(),
        queued: FxHashSet::default(),
        ncap: FxHashMap::default(),
        shapes: Shapes::default(),
        joins: Joins::default(),
        applied: FxHashMap::default(),
        done: FxHashSet::default(),
        below: FxHashMap::default(),
        fed: FxHashMap::default(),
        memo: Memo::default(),
    };
    for &r in roots {
        ensure(cx, &mut run, r);
    }
    // Summaries only grow, and each is a finite set (parameters, keys the program writes,
    // argument shapes over the program's types and functions), and an event applies only what
    // grew since it last applied a summary, so the worklist empties in time linear in the
    // events and the summaries' entries.
    while let Some(d) = run.work.pop_front() {
        run.queued.remove(&d);
        let pending = run.dirty.remove(&d).unwrap_or_default();
        summarize(cx, &mut run, d, pending);
    }
    run.fed
        .into_iter()
        .map(|(f, (site, _))| (f, site))
        .collect()
}

/// Prepare function `d` and queue all its events, the first time it is needed: a function of
/// the program or of a package (std's bodies are not followed).
fn ensure(cx: &mut Ctx, run: &mut Run, d: DefId) {
    // A body being summarized is out of `prepared` meanwhile: `known` remembers it.
    if !run.may.contains(&d) || !run.known.insert(d) {
        return;
    }
    let Some(Def::Fn(f)) = cx.defs[d.0 as usize].take() else {
        return;
    };
    let p = prepare(cx, run, &f);
    run.ncap.insert(d, f.captures.len() as u32);
    cx.defs[d.0 as usize] = Some(Def::Fn(f));
    (run.dirty.entry(d).or_default().events).extend(0..p.events.len() as u32);
    run.prepared.insert(d, p);
    run.sums.entry(Node::Fn(d)).or_default();
    if run.queued.insert(d) {
        run.work.push_back(d);
    }
}

/// Feed the fields `key` names of a value of type `ty` (module docs), first fed at `site`.
fn feed(cx: &mut Ctx, run: &mut Run, ty: TyId, key: &Key, site: &Site) {
    if !run.done.insert((ty, key.clone())) {
        return;
    }
    let ty = match cx.ty.kind(ty) {
        TyKind::Option(inner, _) => *inner,
        _ => ty,
    };
    let mut types = vec![];
    let mut known = true;
    if let Some(d) = adt_of(cx, ty) {
        types.push(d);
    } else if let (TyKind::Dyn(iface, _), None) = (cx.ty.kind(ty), cx.dynamic(ty)) {
        let iface = *iface;
        for i in cx.impls.iter().filter(|i| i.iface == iface) {
            types.extend(adt_of(cx, i.ty));
        }
    } else {
        known = false;
        types.extend(super::targets::held_adts(cx, run.lits));
    }
    for d in types {
        let classes = match run.below.get(&d) {
            Some(cs) => cs.clone(),
            None => {
                // A value of class `d` may be of any of its subclasses.
                let cs = run.lits.below(d);
                run.below.insert(d, cs.clone());
                cs
            }
        };
        for c in classes {
            let Some(a) = cx.adt(c) else { continue };
            for (k, fd) in a.fields.iter().enumerate() {
                let hit = match key {
                    Key::Name(n) => fd.name == *n,
                    Key::Any => true,
                };
                if let (true, Some(f)) = (hit, declaring(cx, c, k as u32)) {
                    // The error names a store into a known object over one into any object.
                    match run.fed.get(&f) {
                        Some((_, true)) => {}
                        Some((_, false)) if !known => {}
                        _ => {
                            run.fed.insert(f, (site.clone(), known));
                        }
                    }
                }
            }
        }
    }
}
