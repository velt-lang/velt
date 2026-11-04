//! Type parameter defaults in a generic call (`function f<T = {}>(…)`).
//!
//! - [`FnCx::default_slots`]: slots nothing inferred take their defaults once every argument is
//!   checked.
//! - [`FnCx::fix_for_context_sensitive`]: before an arrow function with an unannotated parameter
//!   is checked, the type parameters in those parameters' types that nothing inferred yet are
//!   fixed, as TypeScript fixes them before checking a context-sensitive function: sigx's
//!   `component(({ signal }) => …)` types `ctx` as `ComponentSetupContext<…, {}, …>` from
//!   `component<TCombined = {}, …>`. A type parameter only in the function's result still comes
//!   from what the arrow returns, and so does one in the result and in a position an arrow leaves
//!   out (`both(() => 1)`).

use velt_syntax::ast;

use super::args::Callable;
use super::error_slots::{error_only, occurs_plain, union_member_only};
use super::method_value::strip_parens;
use crate::body::FnCx;
use crate::hir::{TyId, TyKind};

impl FnCx<'_, '_> {
    /// Slots nothing inferred take their defaults (while the slots before them are known); an
    /// error type parameter nothing fixed (it appears only as a promise's or function's error
    /// type) is `never`.
    pub(super) fn default_slots(&mut self, c: &Callable, slots: &mut [Option<TyId>]) {
        for (k, slot) in slots.iter_mut().enumerate() {
            if slot.is_some() || c.defaults.get(k).is_some_and(|d| d.is_some()) {
                continue;
            }
            let tys: Vec<TyId> = c.params.iter().map(|p| p.ty).collect();
            let unconstrained_error = tys.iter().any(|t| error_only(self.cx, *t, k as u32))
                && !tys.iter().any(|t| occurs_plain(self.cx, *t, k as u32));
            // A type parameter only ever a member of a parameter's union (`x: T | string`)
            // that the arguments left open (`f("s")`, which the `string` member takes) holds
            // nothing here: TypeScript infers `unknown`, under which `T | string` is `unknown`;
            // `never` keeps it `string`.
            let mut member_only = || {
                let mut any_member = false;
                for t in &tys {
                    match union_member_only(self.cx, *t, k as u32) {
                        Some(m) => any_member |= m,
                        None => return false,
                    }
                }
                any_member
            };
            if unconstrained_error || member_only() {
                *slot = Some(self.cx.ty.never);
            }
        }
        for k in 0..slots.len() {
            let Some(Some(d)) = c.defaults.get(k) else {
                continue;
            };
            if slots[k].is_some() {
                continue;
            }
            let Some(before) = slots[..k].iter().copied().collect::<Option<Vec<TyId>>>() else {
                break;
            };
            slots[k] = Some(self.cx.subst(*d, &before));
        }
        // An overload signature calling its implementation: a type parameter of the
        // implementation that the signature's parameters leave open is `void` (TypeScript
        // infers `unknown`), as in `function f(x: string): string;` over
        // `function f<T>(x: string | T[]): string | T`.
        if slots.iter().any(Option::is_none) && self.signature_body().is_some() {
            for slot in slots.iter_mut().filter(|s| s.is_none()) {
                *slot = Some(self.cx.ty.unit);
            }
        }
    }

    /// Fix what `arg`'s context-sensitive arrows need before they are checked against `pty`:
    /// each slot in the parameter types they leave unannotated that nothing inferred, and the
    /// slots its default names, in order. A slot with a default takes it, one with a constraint
    /// the constraint, and any other is `unknown`, TypeScript's result for no candidates
    /// (`slots` and `known`, the types the arrow is checked with, both get it).
    pub(super) fn fix_for_context_sensitive(
        &mut self,
        c: &Callable,
        arg: &ast::Expr,
        pty: TyId,
        slots: &mut [Option<TyId>],
        known: &mut [Option<TyId>],
    ) {
        if known.iter().all(Option::is_some) {
            return;
        }
        let mut open = OpenSlots::default();
        self.open_slots(arg, pty, known, &mut open);
        if open.fix.is_empty() {
            return;
        }
        let mut fix = open.fix;
        let mut at = 0;
        while at < fix.len() {
            if let Some(Some(d)) = c.defaults.get(fix[at] as usize) {
                crate::types::collect_params(&self.cx.ty, *d, &mut fix);
            }
            at += 1;
        }
        fix.sort_unstable();
        for k in fix {
            let k = k as usize;
            if k >= known.len() || known[k].is_some() || open.keep.contains(&(k as u32)) {
                continue;
            }
            let Some(t) = self.nothing_inferred(c, k, known) else {
                continue;
            };
            known[k] = Some(t);
            slots[k] = Some(t);
        }
    }

    /// A callback returning an array without elements, where the function type's result is `U[]`
    /// or `U | U[]`, fixes an open `U` to `never` (`xs.flatMap((x) => { return []; })`), as
    /// TypeScript's only candidate for it is `never`.
    pub(super) fn never_element(&mut self, pat: TyId, actual: TyId, slots: &mut [Option<TyId>]) {
        let (TyKind::FnPtr { ret: want, .. }, TyKind::FnPtr { ret: got, .. }) = (
            self.cx.ty.kind(pat).clone(),
            self.cx.ty.kind(actual).clone(),
        ) else {
            return;
        };
        if !self
            .cx
            .ty
            .array_elem(got)
            .is_some_and(|e| self.cx.ty.is_bottom(e))
        {
            return;
        }
        let members = self.cx.union_members(want).unwrap_or_else(|| vec![want]);
        for m in members {
            if let Some(&TyKind::Param(k)) = self.cx.ty.array_elem(m).map(|e| self.cx.ty.kind(e)) {
                if let Some(slot @ None) = slots.get_mut(k as usize) {
                    *slot = Some(self.cx.ty.never);
                }
            }
        }
    }

    /// What slot `k`, which no candidate reached, is: `None` when that still depends on an open
    /// slot, or when the slot's interface bound or its place as an error type decides later.
    fn nothing_inferred(&mut self, c: &Callable, k: usize, known: &[Option<TyId>]) -> Option<TyId> {
        let written = match (c.defaults.get(k), c.constraints.get(k)) {
            (Some(Some(d)), _) => Some(*d),
            (_, Some(Some(b))) => Some(*b),
            _ => None,
        };
        if let Some(t) = written {
            let mut names = vec![];
            crate::types::collect_params(&self.cx.ty, t, &mut names);
            let open = names
                .iter()
                .any(|&n| !known.get(n as usize).is_some_and(|s| s.is_some()));
            return (!open).then(|| self.cx.subst_known(t, known));
        }
        let error_slot = c.params.iter().any(|p| error_only(self.cx, p.ty, k as u32));
        if error_slot || c.bounds.get(k).is_some_and(|b| !b.is_empty()) {
            return None;
        }
        let defs = self.cx.any?;
        Some(self.cx.ty.intern(TyKind::Dyn(defs.unknown, vec![])))
    }

    /// The slots the context-sensitive arrows in `arg`, checked against `pty`, need fixed:
    /// directly, inside an array or object literal, or returned by another arrow.
    fn open_slots(
        &mut self,
        arg: &ast::Expr,
        pty: TyId,
        known: &mut [Option<TyId>],
        open: &mut OpenSlots,
    ) {
        let core = self.cx.ty.core(pty).unwrap_or(pty);
        match &strip_parens(arg).kind {
            ast::ExprKind::Arrow {
                type_params,
                params,
                body,
                ..
            } => {
                let TyKind::FnPtr {
                    params: ptys, ret, ..
                } = self.cx.ty.kind(core).clone()
                else {
                    return;
                };
                let sensitive = params.iter().any(|p| p.ty.is_none());
                let mut undeclared = vec![];
                for (i, t) in ptys.iter().enumerate() {
                    match params.get(i) {
                        // An annotated parameter is a candidate first (`(x: number, y) => …` for
                        // `(x: T, y: T) => …`), as in TypeScript.
                        Some(ast::ArrowParam { ty: Some(w), .. }) => {
                            if sensitive && type_params.is_empty() {
                                let before = self.cx.diags.len();
                                let written = self.resolve(w);
                                self.cx.diags.truncate(before);
                                self.cx.match_ty(*t, written, known);
                            }
                        }
                        // Its default types it when the context has nothing.
                        Some(p) if p.default.is_some() => {}
                        Some(_) => crate::types::collect_params(&self.cx.ty, *t, &mut open.fix),
                        None => crate::types::collect_params(&self.cx.ty, *t, &mut undeclared),
                    }
                }
                // A position the arrow doesn't declare types nothing in it: what the result also
                // names comes from what the arrow returns (`both(() => 1)`), the rest has no
                // other source.
                let mut in_result = vec![];
                crate::types::collect_params(&self.cx.ty, ret, &mut in_result);
                // That has to be known before the arrow is checked against the function type: try
                // an arrow that can be checked on its own.
                let from_result = |s: &u32| {
                    in_result.contains(s) && known.get(*s as usize).is_some_and(|k| k.is_none())
                };
                if undeclared.iter().any(from_result) && !sensitive {
                    if let Some(found) = self.trial_type(arg) {
                        if let TyKind::FnPtr { ret: returned, .. } = *self.cx.ty.kind(found) {
                            self.cx.match_ty(ret, returned, known);
                        }
                    }
                }
                open.fix
                    .extend(undeclared.into_iter().filter(|s| !in_result.contains(s)));
                if let ast::ArrowBody::Expr(e) = body {
                    self.open_slots(e, ret, known, open);
                }
            }
            ast::ExprKind::Array(xs) => {
                if let Some(elem) = self.cx.ty.array_elem(core) {
                    for x in xs {
                        self.open_slots(x, elem, known, open);
                    }
                }
            }
            ast::ExprKind::Object(props) => {
                for prop in props {
                    let ast::ObjectProp::KeyValue(key, value) = prop else {
                        continue;
                    };
                    let Some((_, t)) = self.field_of(core, &key.name) else {
                        continue;
                    };
                    if context_sensitive(value) {
                        self.open_slots(value, t, known, open);
                    } else {
                        // A sibling is a candidate first (`{ cb: (x) => …, v: 1 }`), and without
                        // a type of its own it is checked with the literal.
                        let mut names = vec![];
                        crate::types::collect_params(&self.cx.ty, t, &mut names);
                        if names.is_empty() {
                            continue;
                        }
                        if let Some(found) = self.trial_type(value) {
                            self.cx.match_ty(t, found, known);
                        }
                        open.keep.extend(names);
                    }
                }
            }
            _ => {}
        }
    }
}

/// The slots [`FnCx::fix_for_context_sensitive`] looks at.
#[derive(Default)]
struct OpenSlots {
    /// In the parameter types of a context-sensitive arrow.
    fix: Vec<u32>,
    /// Also in a sibling of that arrow in an object literal, which infers them.
    keep: Vec<u32>,
}

/// Does `e` need its type from the parameter it is passed for: an arrow with a parameter neither
/// annotated nor defaulted, or an array or object literal or arrow result holding one? TypeScript
/// checks these after every other argument.
pub(super) fn context_sensitive(e: &ast::Expr) -> bool {
    match &strip_parens(e).kind {
        ast::ExprKind::Arrow { params, body, .. } => {
            params.iter().any(|p| p.ty.is_none() && p.default.is_none())
                || matches!(body, ast::ArrowBody::Expr(b) if context_sensitive(b))
        }
        ast::ExprKind::Array(xs) => xs.iter().any(context_sensitive),
        ast::ExprKind::Object(props) => props
            .iter()
            .any(|p| matches!(p, ast::ObjectProp::KeyValue(_, v) if context_sensitive(v))),
        _ => false,
    }
}
