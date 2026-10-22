//! What a body's values are derived from, for the rule that a part of a value read out of a
//! `shared` is never changed (#708): JavaScript would change the shared value, and Velt's
//! reader works on a copy or on memory other threads read. Instead of naming each way a part
//! can travel, everything a value may be or contain is followed: a binding, an assignment, a
//! loop element (also of a map's cursor loop and the iterator protocol, `Ctx::loop_roots`), a
//! branch of a conditional or `match`, and what a method returns. A new container (an array,
//! tuple or object literal, a constructor's arguments, a std copy such as `slice()`) and a field
//! assignment's target only *hold* what goes in: a change to their own slots changes nothing
//! else, a change through them (`Evidence::deep`) changes what they hold. A change to a value derived from a
//! parameter counts as a change of the parameter (`FnInfo::modified_params`); one to a value
//! derived from a `shared` read is an error (`finish::frozen_bindings`, `patch`).

use crate::ctx::Ctx;
use crate::hir::StmtKind as S;
use crate::hir::{Callee, Expr, ExprKind as E, FnDef, Intrinsic, LocalId, Pat, PatKind, Stmt};
use crate::visit::{self, VisitMut};

/// The roots of a value: the locals it may be a part of, whether it may be a part of a value
/// read out of a `shared`, and what it only holds (a new container or literal with parts in it:
/// `[v]`, `{ ...v }`, `new Map(m)`, `xs.slice()`), whose own slots may change freely.
#[derive(Default)]
pub(super) struct Roots {
    pub locals: Vec<LocalId>,
    pub frozen: bool,
    pub held: Vec<LocalId>,
    pub held_frozen: bool,
}

/// The roots of the value `e` evaluates to.
pub(super) fn roots(cx: &Ctx, e: &Expr) -> Roots {
    let mut r = Roots::default();
    walk(cx, e, &mut r, false, 0);
    r
}

/// The roots of an element (or field) of the value `e` evaluates to, `levels` deep:
/// `[[v]][0][0]` is `v` itself.
pub(super) fn elem_roots(cx: &Ctx, e: &Expr) -> Roots {
    let mut r = Roots::default();
    elems(cx, e, &mut r, false, 1, 0);
    r
}

fn walk(cx: &Ctx, e: &Expr, r: &mut Roots, holds: bool, depth: u32) {
    if depth > 64 {
        return;
    }
    let go = |x: &Expr, r: &mut Roots, holds: bool| walk(cx, x, r, holds, depth + 1);
    match &e.kind {
        E::Local(l, _) => match holds {
            true => r.held.push(*l),
            false => r.locals.push(*l),
        },
        E::Call {
            callee: Callee::Intrinsic(Intrinsic::SharedRead),
            ..
        } => match holds {
            true => r.held_frozen = true,
            false => r.frozen = true,
        },
        E::Call {
            callee: Callee::Intrinsic(Intrinsic::Share),
            args,
        } => args.iter().for_each(|a| go(a, r, holds)),
        // An element taken out of a new container is a part again (`[v][0]`, `xs.slice()[0]`).
        E::Field { base, .. } | E::Index { base, .. } => elems(cx, base, r, holds, 1, depth + 1),
        E::UnwrapSome(base, _)
        | E::UnwrapVariant { expr: base, .. }
        | E::WrapSome(base)
        | E::Downcast(base)
        | E::Upcast(base) => go(base, r, holds),
        E::If { then, els, .. } => {
            go(then, r, holds);
            go(els, r, holds);
        }
        E::Logical { lhs, rhs, .. } => {
            go(lhs, r, holds);
            go(rhs, r, holds);
        }
        E::Match { scrutinee, arms } => {
            go(scrutinee, r, holds);
            arms.iter().for_each(|a| go(&a.body, r, holds));
        }
        E::Block(b) => b.value.iter().for_each(|v| go(v, r, holds)),
        // A new container holds its elements; a value without references in it (a number read
        // out of the shared value) holds no part.
        E::ArrayLit(es) | E::Tuple(es) => es.iter().for_each(|x| {
            if !leaf(cx, x.ty) {
                go(x, r, true)
            }
        }),
        E::AdtLit { fields: es, .. } | E::Variant { args: es, .. } | E::New { args: es, .. } => es
            .iter()
            .filter(|x| !leaf(cx, x.ty))
            .for_each(|x| go(x, r, true)),
        E::Call {
            callee: Callee::Def(d, _),
            args,
        } if cx.fn_info(*d).this.is_some() && !builds(cx, *d) => {
            // A std copy is a new container holding the receiver's parts.
            let holds = holds || copies(cx, *d);
            args.first().iter().for_each(|a| go(a, r, holds))
        }
        E::Call {
            callee: Callee::Virtual { .. } | Callee::Dyn { .. },
            args,
        } => args.first().iter().for_each(|a| go(a, r, holds)),
        _ => {}
    }
}

/// The roots of what an element or field of `e` is, `levels` deep (`levels` > 0): for a new
/// container (a literal, a constructor's arguments, a std copy) those of the parts it holds,
/// else what `e` is part of.
fn elems(cx: &Ctx, e: &Expr, r: &mut Roots, holds: bool, levels: u32, depth: u32) {
    if depth > 64 {
        return;
    }
    let inner = |x: &Expr, r: &mut Roots| match levels {
        1 => walk(cx, x, r, holds, depth + 1),
        _ => elems(cx, x, r, holds, levels - 1, depth + 1),
    };
    match &e.kind {
        E::ArrayLit(es) | E::Tuple(es) => es.iter().for_each(|x| inner(x, r)),
        E::AdtLit { fields: es, .. } | E::Variant { args: es, .. } | E::New { args: es, .. } => {
            es.iter().for_each(|x| inner(x, r))
        }
        E::Field { base, .. } | E::Index { base, .. } => {
            elems(cx, base, r, holds, levels + 1, depth + 1)
        }
        E::Call {
            callee: Callee::Def(d, _),
            args,
        } if cx.fn_info(*d).this.is_some() && copies(cx, *d) => args
            .first()
            .iter()
            .for_each(|a| elems(cx, a, r, holds, levels, depth + 1)),
        _ => walk(cx, e, r, holds, depth),
    }
}

/// Does a value of type `t` hold no references (a number, a string, a boolean)?
fn leaf(cx: &Ctx, t: crate::hir::TyId) -> bool {
    use crate::hir::TyKind as T;
    match cx.ty.kind(t) {
        T::Int(_) | T::Float(_) | T::Bool | T::Str | T::Literal(_) | T::Unit | T::Never => true,
        T::Option(x, _) => leaf(cx, *x),
        _ => false,
    }
}

/// Is `e` a new container (a literal, a constructor call, a std copy): a write to its own slot
/// changes nothing anything else holds?
pub(super) fn fresh(cx: &Ctx, e: &Expr) -> bool {
    match &e.kind {
        E::ArrayLit(_) | E::Tuple(_) | E::AdtLit { .. } | E::Variant { .. } | E::New { .. } => true,
        E::Call {
            callee: Callee::Def(d, _),
            ..
        } => cx.fn_info(*d).this.is_some() && (copies(cx, *d) || builds(cx, *d)),
        _ => false,
    }
}

/// Is `d` a std method that returns a new container of what its callback returns (`map`)?
/// What that is isn't followed: `xs.map((x) => x)` holds the receiver's parts unseen.
fn builds(cx: &Ctx, d: crate::hir::DefId) -> bool {
    let name = &cx.fn_info(d).name;
    name.starts_with("std/") && [".map", ".flatMap"].iter().any(|m| name.ends_with(m))
}

/// Is `d` a std method that returns a new container of the receiver's parts (`slice()`, a
/// map's `entries()`)?
fn copies(cx: &Ctx, d: crate::hir::DefId) -> bool {
    let name = &cx.fn_info(d).name;
    name.starts_with("std/")
        && [
            "slice",
            "concat",
            "filter",
            "flat",
            "toSorted",
            "toReversed",
            "toSpliced",
            "with",
            "entries",
            "keys",
            "values",
        ]
        .iter()
        .any(|m| name.ends_with(&format!(".{m}")))
}

/// That `bound` gets a part of `src` (`None`: of a value read out of a `shared`), or with
/// `held` only holds it.
pub(super) struct Edge {
    pub bound: LocalId,
    pub src: Option<LocalId>,
    pub held: bool,
    /// What `bound` gets is projected out of the source (`a[0]`, `b.it`, a loop element): a
    /// part of what the source holds, not the holder itself.
    pub projected: bool,
}

/// Each local a statement or expression of `f`'s body binds or assigns, with what it gets a
/// part of or holds (loop elements included).
pub(super) fn edges(cx: &mut Ctx, f: &mut FnDef) -> Vec<Edge> {
    let mut v = Edges { cx, out: vec![] };
    visit::block(&mut f.body.block, &mut v);
    let mut out = vec![];
    for (bound, r, ty, projected) in v.out {
        let local = &f.body.locals[bound.0 as usize];
        // The iterator protocol's hidden locals change as the loop steps; its elements are
        // tied to the loop's source directly (`Ctx::loop_roots`). A value without references
        // holds no part of anything.
        let stepping = ["<cursor", "<iterator", "<generator", "<result"];
        if stepping.iter().any(|h| local.name.starts_with(h))
            || cx.is_copy(local.ty)
            || cx.is_copy(ty)
        {
            continue;
        }
        let edge = |src, held| Edge {
            bound,
            src,
            held,
            projected,
        };
        out.extend(r.locals.iter().map(|&x| edge(Some(x), false)));
        out.extend(r.held.iter().map(|&x| edge(Some(x), true)));
        if r.frozen {
            out.push(edge(None, false));
        }
        if r.held_frozen {
            out.push(edge(None, true));
        }
    }
    for (i, l) in f.body.locals.iter().enumerate() {
        if let Some(&root) = cx.loop_roots.get(&l.span) {
            out.push(Edge {
                bound: LocalId(i as u32),
                src: Some(root),
                held: false,
                projected: true,
            });
        }
    }
    out
}

struct Edges<'a, 'm> {
    cx: &'a Ctx<'m>,
    /// Each binding, with the roots of what it gets, that value's type, and whether it is
    /// projected out of its source (`Edge::projected`).
    out: Vec<(LocalId, Roots, crate::hir::TyId, bool)>,
}

impl Edges<'_, '_> {
    fn bind(&mut self, bound: Vec<LocalId>, from: &Expr) {
        self.bind_as(bound, from, projected(from));
    }

    fn bind_as(&mut self, bound: Vec<LocalId>, from: &Expr, projected: bool) {
        for l in bound {
            self.out.push((l, roots(self.cx, from), from.ty, projected));
        }
    }

    /// `bound` get elements or fields of `from` (a destructuring pattern, a loop element).
    fn bind_elems(&mut self, bound: Vec<LocalId>, from: &Expr) {
        for l in bound {
            self.out.push((l, elem_roots(self.cx, from), from.ty, true));
        }
    }
}

impl VisitMut for Edges<'_, '_> {
    fn stmt(&mut self, s: &mut Stmt) {
        match &s.kind {
            S::Let {
                local,
                init: Some(init),
            } => self.bind(vec![*local], init),
            S::LetPat { pat, init } => match pat.kind {
                PatKind::Binding(..) => self.bind(pat_locals(pat), init),
                _ => self.bind_elems(pat_locals(pat), init),
            },
            S::ForOf { binding, iter, .. } => self.bind_elems(pat_locals(binding), iter),
            _ => {}
        }
    }

    fn expr(&mut self, e: &mut Expr) {
        match &e.kind {
            E::Assign { place, value } => match place.kind {
                E::Local(l, _) => self.bind(vec![l], value),
                // `this.i = i`: what the place is a part of now holds what the value is, so a
                // change through it may change that.
                _ => {
                    for l in roots(self.cx, place).locals {
                        let v = roots(self.cx, value);
                        let held = Roots {
                            held: v.locals.into_iter().chain(v.held).collect(),
                            held_frozen: v.frozen || v.held_frozen,
                            ..Roots::default()
                        };
                        self.out.push((l, held, value.ty, false));
                    }
                }
            },
            E::Match { scrutinee, arms } => {
                for a in arms {
                    self.bind_as(pat_locals(&a.pat), scrutinee, true);
                }
            }
            _ => {}
        }
    }
}

/// Is `e` taken out of what it is part of (a field, an element, an unwrapped payload)?
fn projected(e: &Expr) -> bool {
    match &e.kind {
        E::Field { .. } | E::Index { .. } | E::UnwrapSome(..) | E::UnwrapVariant { .. } => true,
        E::Upcast(x) | E::Downcast(x) => projected(x),
        // Any branch may be the projection.
        E::If { then, els, .. } => projected(then) || projected(els),
        E::Logical { lhs, rhs, .. } => projected(lhs) || projected(rhs),
        E::Match { arms, .. } => arms.iter().any(|a| projected(&a.body)),
        E::Block(b) => b.value.as_deref().is_some_and(projected),
        E::Call {
            callee: Callee::Intrinsic(Intrinsic::Share),
            args,
        } => args.first().is_some_and(projected),
        _ => false,
    }
}

/// The locals pattern `p` binds.
pub(super) fn pat_locals(p: &Pat) -> Vec<LocalId> {
    fn go(p: &Pat, out: &mut Vec<LocalId>) {
        match &p.kind {
            PatKind::Binding(l, _) => out.push(*l),
            PatKind::Variant { args: ps, .. } | PatKind::Tuple(ps) | PatKind::Or(ps) => {
                ps.iter().for_each(|x| go(x, out))
            }
            PatKind::Array { elems, rest } => {
                elems.iter().for_each(|x| go(x, out));
                out.extend(rest.iter().copied());
            }
            PatKind::Adt { fields } => fields.iter().for_each(|(_, x)| go(x, out)),
            PatKind::Some(x) => go(x, out),
            PatKind::Wildcard | PatKind::Lit(_) | PatKind::None | PatKind::InstanceOf(_) => {}
        }
    }
    let mut out = vec![];
    go(p, &mut out);
    out
}
