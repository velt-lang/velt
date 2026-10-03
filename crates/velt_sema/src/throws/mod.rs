//! Typed errors (docs/reference/errors.md): what every function, `try` block and function value can
//! throw.
//!
//! - A function's error type is the canonical union of the types it `throw`s outside a `try`
//!   with a `catch`, and of what the functions it calls there throw ([`sets`]). It is written
//!   with a `throws` clause or inferred over the whole call graph (a fixpoint, so recursion
//!   works; [`infer`]). A written clause bounds the body and fixes the error type.
//! - Functions dispatched through one entry (interface slots, vtable slots) share one error type
//!   ([`groups`]); closures get the error type of the function type they are created for (or,
//!   without one, what their body throws), because a function value's type carries it
//!   (`TyKind::FnPtr::throws`); a promise's type carries its rejection type.
//! - While bodies are checked, types that depend on errors (a `catch` variable, a function
//!   value's type, a promise's type) are computed from what is known ([`throws_now`], checking
//!   callee bodies on demand; a cycle counts as not throwing yet). They are re-checked against
//!   the final inference ([`checks`]): recursion may need a `throws` clause.

mod checks;
mod conventions;
mod groups;
mod infer;
mod sets;

use std::collections::{HashMap, HashSet};

use velt_common::Span;

pub(crate) use groups::Groups;
pub(crate) use infer::infer_all;

use crate::ctx::Ctx;
use crate::defs::ThrowSrc;
use crate::hir::{DefId, TyId};

/// What a resolution of throw sources has already counted.
#[derive(Default)]
struct Visited {
    /// Functions whose bodies' sources are counted (in their own generic context).
    fns: HashSet<DefId>,
    inits: InitsSeen,
}

/// How many instantiations of one class's field initializers a resolution counts separately.
/// Initializers can construct their class with ever larger type arguments (`Box<T>` running
/// `new Box<Box<T>>()`), which would never end; past this many, further instantiations of the
/// class count as already seen (its first ones count). Ordinary programs construct a class with
/// a few distinct type arguments in one chain of initializers.
const MAX_INIT_INSTANCES: usize = 16;

/// The classes whose field initializers ([`ThrowSrc::Defaults`]) are already counted, keyed on
/// the class and its type arguments: `Box<E1>` and `Box<E2>` throw different errors.
#[derive(Default)]
pub(crate) struct InitsSeen(HashMap<DefId, HashSet<Vec<TyId>>>);

impl InitsSeen {
    /// Record class `d` with type arguments `args`: false when they are counted already (or
    /// the class has [`MAX_INIT_INSTANCES`] counted).
    pub(crate) fn insert(&mut self, d: DefId, args: &[TyId]) -> bool {
        let seen = self.0.entry(d).or_default();
        seen.len() < MAX_INIT_INSTANCES && seen.insert(args.to_vec())
    }
}

/// A type sema built from what `srcs` throw at checking time; the final inference must agree
/// (`exact`: equal, for promises whose layout depends on it; else the final type may be smaller).
#[derive(Clone, Debug)]
pub(crate) struct ThrowCheck {
    pub srcs: Vec<ThrowSrc>,
    pub observed: Option<TyId>,
    pub exact: bool,
    pub span: Span,
}

/// What calling `d` with type args `targs` throws, from what is known now.
pub(crate) fn throws_now(cx: &mut Ctx, d: DefId, targs: &[TyId]) -> Option<TyId> {
    let mut visited = Visited::default();
    let t = def_now(cx, d, &mut visited);
    subst_error(cx, t, targs)
}

/// What the throw sources `srcs` throw, from what is known now.
pub(crate) fn srcs_now(cx: &mut Ctx, srcs: &[ThrowSrc]) -> Option<TyId> {
    let mut visited = Visited::default();
    srcs_now_in(cx, srcs, &mut visited)
}

fn srcs_now_in(cx: &mut Ctx, srcs: &[ThrowSrc], visited: &mut Visited) -> Option<TyId> {
    let mut acc = None;
    for s in srcs {
        let t = match s {
            ThrowSrc::Direct(t, _) => Some(*t),
            ThrowSrc::Call(g, targs, _) => {
                let t = def_now(cx, *g, visited);
                subst_error(cx, t, targs)
            }
            ThrowSrc::Slot {
                iface, slot, args, ..
            } => {
                let t = slot_now(cx, *iface, *slot, visited);
                subst_error(cx, t, args)
            }
            ThrowSrc::Defaults(d, args, span) => {
                if !visited.inits.insert(*d, args) {
                    continue;
                }
                let srcs = defaults_srcs(cx, *d, args, *span);
                srcs_now_in(cx, &srcs, visited)
            }
        };
        acc = cx.join_errors(acc, t);
    }
    acc
}

/// The throw sources of the own field initializers of class `d` (in the context of `args`),
/// as run by a `new` or a constructor at `span`. An initializer may itself construct a class
/// ([`ThrowSrc::Defaults`]); resolving those with a visited set computes a cycle's errors as a
/// fixpoint (the union over every class reachable from `d`).
pub(crate) fn defaults_srcs(cx: &mut Ctx, d: DefId, args: &[TyId], span: Span) -> Vec<ThrowSrc> {
    crate::body::field_defaults(cx, d);
    let Some(a) = cx.adt(d) else {
        return vec![];
    };
    let own: Vec<ThrowSrc> = a.fields[a.own_fields_start..]
        .iter()
        .flat_map(|f| f.default_throws.iter().cloned())
        .collect();
    own.iter()
        .map(|s| s.used_at(span, |t| cx.ty.subst(t, args)))
        .collect()
}

fn subst_error(cx: &mut Ctx, t: Option<TyId>, targs: &[TyId]) -> Option<TyId> {
    let t = t.map(|t| cx.ty.subst(t, targs));
    cx.canon_error(t)
}

/// `d`'s error type in its own generic context.
fn def_now(cx: &mut Ctx, d: DefId, visited: &mut Visited) -> Option<TyId> {
    let f = cx.try_fn(d)?;
    if let Some(decl) = f.declared_throws {
        return decl.ty;
    }
    match cx.throw_groups().group_of(d) {
        Some(g) => group_now(cx, g, visited),
        None => own_now(cx, d, visited),
    }
}

/// What `d`'s body throws (its body is checked on demand).
fn own_now(cx: &mut Ctx, d: DefId, visited: &mut Visited) -> Option<TyId> {
    if !visited.fns.insert(d) {
        return None;
    }
    if let Some(decl) = cx.try_fn(d).and_then(|f| f.declared_throws) {
        return decl.ty;
    }
    crate::body::ensure_body(cx, d);
    let srcs = cx.try_fn(d)?.throw_srcs.clone();
    srcs_now_in(cx, &srcs, visited)
}

fn group_now(cx: &mut Ctx, g: usize, visited: &mut Visited) -> Option<TyId> {
    let group = cx.throw_groups().list[g].clone();
    if let Some(b) = group.bound {
        return b.decl.ty;
    }
    let mut acc = None;
    for m in group.members {
        let t = own_now(cx, m, visited).filter(|t| !cx.mentions_params(*t));
        acc = cx.join_errors(acc, t);
    }
    acc
}

fn slot_now(cx: &mut Ctx, iface: DefId, slot: u32, visited: &mut Visited) -> Option<TyId> {
    let g = cx.throw_groups().slot_group(iface, slot)?;
    group_now(cx, g, visited)
}

impl Ctx<'_> {
    /// The dispatch groups (built on first use; interfaces, impls and vtables are known once
    /// declarations are collected).
    pub(crate) fn throw_groups(&mut self) -> &Groups {
        if self.groups.is_none() {
            let g = groups::build(self);
            self.groups = Some(g);
        }
        self.groups.as_ref().expect("ICE: groups built")
    }
}
