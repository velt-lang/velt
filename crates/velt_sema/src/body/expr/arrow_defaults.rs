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

use super::args::{as_arrow, Callable};
use super::error_slots::{error_only, occurs_plain, union_member_only};
use super::method_value::strip_parens;
use super::ops::untyped;
use super::setters::synth;
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
            let Some(t) = self.nothing_inferred(c, k, known, arg.span) else {
                continue;
            };
            known[k] = Some(t);
            slots[k] = Some(t);
        }
    }

    /// An arrow's annotated parameter fixes a slot before a literal argument for that slot, when
    /// the literal has no type of its own to offer (`xs.reduce((acc: number[], x) => …, [])`, with
    /// `null`, `{}`, `new Map()`), or when what the arrow returns doesn't fit the literal's type
    /// (`fold((a: number | string) => a, 1)`). TypeScript prefers what a value gives over what a
    /// parameter takes (`on((x: number | string) => {}, 1)` and `fold((a: number | string) => 2, 1)`
    /// are `number`s), but what a result gives is a value too, and the wider candidate wins.
    /// Callbacks whose annotations for one slot differ leave it to the literal.
    pub(super) fn annotations_before_literals(
        &mut self,
        c: &Callable,
        args: &[ast::Expr],
        slots: &mut [Option<TyId>],
    ) {
        if slots.iter().all(Option::is_some) || !args.iter().any(|a| as_arrow(a).is_some()) {
            return;
        }
        let mut found: Vec<(usize, TyId, bool)> = vec![];
        for (arg, p) in args.iter().zip(&c.params) {
            let Some(arrow) = as_arrow(arg) else {
                continue;
            };
            let ast::ExprKind::Arrow {
                type_params,
                params,
                ..
            } = &arrow.kind
            else {
                continue;
            };
            if !type_params.is_empty() {
                continue;
            }
            let core = self.cx.ty.core(p.ty).unwrap_or(p.ty);
            let TyKind::FnPtr {
                params: ptys, ret, ..
            } = self.cx.ty.kind(core).clone()
            else {
                continue;
            };
            let mut in_result = vec![];
            crate::types::collect_params(&self.cx.ty, ret, &mut in_result);
            for (t, written) in ptys.iter().zip(params) {
                let Some(written) = &written.ty else {
                    continue;
                };
                // Only a slot a literal argument is passed for directly.
                let TyKind::Param(k) = *self.cx.ty.kind(*t) else {
                    continue;
                };
                if slots.get(k as usize).is_none_or(Option::is_some) {
                    continue;
                }
                let literals: Vec<&ast::Expr> = (args.iter().zip(&c.params))
                    .filter(|(_, p)| matches!(*self.cx.ty.kind(p.ty), TyKind::Param(s) if s == k))
                    .map(|(a, _)| a)
                    .collect();
                let before = self.cx.diags.len();
                let resolved = self.resolve(written);
                self.cx.diags.truncate(before);
                let mut takes = false;
                for a in literals {
                    takes |= self.has_no_type_of_its_own(a)
                        || (in_result.contains(&k)
                            && literal_arg(a)
                            && self.result_needs_annotation(
                                (c, arrow, core),
                                (k as usize, resolved),
                                a,
                                slots,
                            ));
                }
                found.push((k as usize, resolved, takes));
            }
        }
        // Only annotations a literal is passed beside count, and they must agree.
        for &(k, t, takes) in &found {
            if !takes {
                continue;
            }
            let agree = found
                .iter()
                .filter(|(other, _, _)| *other == k)
                .all(|&(_, u, _)| u == t || (self.converts_to(u, t) && self.converts_to(t, u)));
            if agree {
                slots[k] = Some(t);
            }
        }
    }

    /// Does what `arrow` returns for slot `k`, checked with `written` for it, not fit the type of
    /// the literal argument for that slot? The check is a trial; one that fails says yes, and the
    /// real check reports what is wrong.
    fn result_needs_annotation(
        &mut self,
        (c, arrow, core): (&Callable, &ast::Expr, TyId),
        (k, written): (usize, TyId),
        literal: &ast::Expr,
        slots: &[Option<TyId>],
    ) -> bool {
        let TyKind::FnPtr { ret, .. } = *self.cx.ty.kind(core) else {
            return true;
        };
        let mut with = slots.to_vec();
        with[k] = Some(written);
        let expected = self.cx.subst_known(core, &with);
        let open_ret = self.cx.subst_known(ret, slots);
        let TyKind::FnPtr {
            params,
            throws,
            rest,
            ..
        } = self.cx.ty.kind(expected).clone()
        else {
            return true;
        };
        // The arrow's result is what the body gives, not the annotation.
        let expected = self.cx.ty.intern(TyKind::FnPtr {
            params,
            ret: open_ret,
            throws,
            rest,
        });
        let notes = match self.cx.ty.kind(core) {
            TyKind::FnPtr { params, .. } if c.js_api => {
                Some(params.iter().map(|t| self.cx.ty.is_int(*t)).collect())
            }
            _ => None,
        };
        let found = self.remembered_trial(arrow, expected, |lw| {
            lw.std_callback = notes;
            lw.closure(arrow, Some(expected), false).ty
        });
        let Some(found) = found else {
            return true;
        };
        let TyKind::FnPtr { ret: returned, .. } = *self.cx.ty.kind(found) else {
            return true;
        };
        self.arrow_tried(arrow, found);
        let mut from_result = slots.to_vec();
        self.cx.match_ty(ret, returned, &mut from_result);
        // Nothing for the slot: the arrow doesn't return a value, which the annotation can't help.
        let Some(given) = from_result[k].filter(|t| *t != self.cx.ty.unit) else {
            return false;
        };
        // A literal value's check changes nothing, so it needs no trial.
        let literal_ty = match &strip_parens(literal).kind {
            ast::ExprKind::Lit(_) => Some(self.expr(literal, None, crate::body::Want::Borrow).ty),
            _ => self.remembered_type(literal),
        };
        let Some(literal_ty) = literal_ty else {
            return true;
        };
        if self.cx.ty.has_error(given) {
            return true;
        }
        let (given, literal_ty) = (self.cx.widened(given), self.cx.widened(literal_ty));
        !self.converts_to(given, literal_ty)
    }

    /// `null`, `undefined`, `[]`, `{}`, or `new C()` of a generic class without type arguments:
    /// nothing in it says what type it is.
    fn has_no_type_of_its_own(&mut self, e: &ast::Expr) -> bool {
        match &strip_parens(e).kind {
            ast::ExprKind::Lit(ast::Lit::Null | ast::Lit::Undefined) => true,
            ast::ExprKind::Array(xs) => xs.is_empty(),
            ast::ExprKind::Object(props) => props.is_empty(),
            ast::ExprKind::New { class, .. } => match &class.kind {
                ast::TypeExprKind::Named { path, args } if args.is_empty() => {
                    match self.lookup_type_path(path) {
                        Some(crate::ctx::Item::Def(d)) => {
                            self.cx.adt(d).is_some_and(|a| !a.generics.names.is_empty())
                        }
                        _ => false,
                    }
                }
                _ => false,
            },
            _ => false,
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

    /// What slot `k`, which no candidate reached, is: its default, its constraint (a type, or
    /// the interfaces it extends), or `unknown`. `None` when that still depends on an open slot,
    /// or when its place as an error type decides later.
    fn nothing_inferred(
        &mut self,
        c: &Callable,
        k: usize,
        known: &[Option<TyId>],
        span: velt_common::Span,
    ) -> Option<TyId> {
        let bounds = c.bounds.get(k).map_or(&[][..], Vec::as_slice);
        let parts: Vec<TyId> = (bounds.iter())
            .map(|b| self.cx.ty.intern(TyKind::Dyn(b.iface, b.args.clone())))
            .collect();
        let interface = match parts.as_slice() {
            [] => None,
            [one] => Some(*one),
            _ => Some(self.cx.intersect_parts(parts, self.module, span)),
        };
        let written = match (c.defaults.get(k), c.constraints.get(k)) {
            (Some(Some(d)), _) => Some(*d),
            (_, Some(Some(b))) => Some(*b),
            _ => interface,
        };
        if let Some(t) = written {
            let mut names = vec![];
            crate::types::collect_params(&self.cx.ty, t, &mut names);
            let open = names
                .iter()
                .any(|&n| !known.get(n as usize).is_some_and(|s| s.is_some()));
            if open {
                return None;
            }
            let t = self.cx.subst_known(t, known);
            // The function may convert an `any` to it (`g({ n: 1 } as any)`), as for an explicit
            // type argument.
            self.cx.note_dyn_object_target(t);
            return Some(t);
        }
        let error_slot = c.params.iter().any(|p| error_only(self.cx, p.ty, k as u32));
        if error_slot {
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
                    if let Some(found) = self.remembered_type(arg) {
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
                // From the last: a later field replaces an earlier one of that name
                // (`{ ...base, v: 1 }`), so it is the candidate.
                for prop in props.iter().rev() {
                    match prop {
                        ast::ObjectProp::KeyValue(key, value) if context_sensitive(value) => {
                            if let Some((_, t)) = self.field_of(core, &key.name) {
                                self.open_slots(value, t, known, open);
                            }
                        }
                        // A sibling is a candidate first (`{ cb: (x) => …, v: 1 }`), and without
                        // a type of its own it is checked with the literal.
                        ast::ObjectProp::KeyValue(key, value) => {
                            self.sibling_candidate(core, &key.name, value, known, open);
                        }
                        ast::ObjectProp::Shorthand(name) => {
                            let value = synth(ast::ExprKind::Ident(name.clone()), name.span);
                            self.sibling_candidate(core, &name.name, &value, known, open);
                        }
                        ast::ObjectProp::Spread(value) => {
                            let Some(found) = self.remembered_type(value) else {
                                continue;
                            };
                            let found = self.cx.ty.core(found).unwrap_or(found);
                            let Some((d, targs)) = self.adt_of(found) else {
                                continue;
                            };
                            let names: Vec<(String, TyId)> = match self.cx.adt(d) {
                                Some(a) => {
                                    a.fields.iter().map(|f| (f.name.clone(), f.ty)).collect()
                                }
                                None => continue,
                            };
                            for (name, from) in names {
                                if let Some((_, t)) = self.field_of(core, &name) {
                                    if open.keep_named(self.cx, t) {
                                        let from = self.cx.subst(from, &targs);
                                        self.cx.match_ty(t, from, known);
                                    }
                                }
                            }
                        }
                        ast::ObjectProp::Method(_) => {}
                    }
                }
            }
            _ => {}
        }
    }

    /// Field `name` of the object type `core` given `value`, next to a context-sensitive arrow:
    /// the type parameters its type names are inferred from `value`.
    fn sibling_candidate(
        &mut self,
        core: TyId,
        name: &str,
        value: &ast::Expr,
        known: &mut [Option<TyId>],
        open: &mut OpenSlots,
    ) {
        let Some((_, t)) = self.field_of(core, name) else {
            return;
        };
        if open.keep_named(self.cx, t) {
            if let Some(found) = self.remembered_type(value) {
                self.cx.match_ty(t, found, known);
            }
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

impl OpenSlots {
    /// Adds the type parameters `t` names to `keep`: are there any?
    fn keep_named(&mut self, cx: &crate::ctx::Ctx<'_>, t: TyId) -> bool {
        let before = self.keep.len();
        crate::types::collect_params(&cx.ty, t, &mut self.keep);
        self.keep.len() > before
    }
}

/// Is `e` a literal, which takes its type from what it is passed for when it can?
fn literal_arg(e: &ast::Expr) -> bool {
    match &strip_parens(e).kind {
        ast::ExprKind::Lit(_)
        | ast::ExprKind::Array(_)
        | ast::ExprKind::NumArray(_)
        | ast::ExprKind::Object(_) => true,
        ast::ExprKind::New { class, .. } => {
            matches!(&class.kind, ast::TypeExprKind::Named { args, .. } if args.is_empty())
        }
        _ => untyped(e),
    }
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
