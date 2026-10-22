//! Module constants evaluated at each use (not kept in a slot: `export const K: J = { … }`)
//! reached through a form that hides them from the place check: `K!`, `K as T`, `K ?? d`,
//! `K || d`, `c ? K : d`, a namespace member (`ns.K`) and, when taken by value, a part of one
//! (`K!.a`, `N.a`). Each use makes a new value, so a write through one of these forms, or a
//! variable taking one over, would change a copy where TypeScript changes the one object: the
//! constant is kept in the module state (`crate::kept_consts::keep`), as for `K.v = 1` and
//! `const v = K`, or, when it can't be (it holds a promise), reported as those are.

use velt_common::Span;
use velt_syntax::ast;

use crate::body::FnCx;
use crate::hir::{DefId, TyId};

use super::assign_paths::strip_parens;

/// How [`FnCx::per_use_root`] follows a part (`x.a`, `x[i]`) of the value.
#[derive(Clone, Copy, PartialEq)]
enum Parts {
    /// A write to a part: the part of `K` itself is the place check's (`K.a.b = 1`).
    Write,
    /// A part taken by value: another reference in TypeScript, a copy here (`N.a`).
    Take,
}

impl FnCx<'_, '_> {
    /// A write into a part of `target` whose object reaches a module constant evaluated at
    /// each use through `!`, `as`, `??`, `||`, `&&` or `?:` (`K!.v = 1`, `(K as any).v = 1`,
    /// `(c ? K! : d).v = 1`, `(ns.K as any).v = 1`): the constant is kept in the module state,
    /// as for `K.v = 1`, or, when it can't be, the write is reported, since it would change a
    /// copy. The source form is followed, whatever the checked target becomes (a conversion to
    /// `any`, a narrowed read).
    pub(super) fn per_use_const_write(&mut self, target: &ast::Expr) -> bool {
        let (object, what) = match &strip_parens(target).kind {
            ast::ExprKind::Member { object, .. } => (object, "assign to a field of"),
            ast::ExprKind::Index { object, .. } => (object, "assign to an element of"),
            _ => return false,
        };
        let Some((d, name, span)) = self.per_use_root(object, false, Parts::Write) else {
            return false;
        };
        // Written, the constant is the one object the module state keeps, as in Node.
        if crate::kept_consts::keep(self.cx, d) {
            return false;
        }
        self.cx.err(
            format!("cannot {what} module-level constant `{name}`"),
            span,
        );
        true
    }

    /// `e`, of type `ty`, taken by value (`const t = (K ?? d)!`, `K!.a`, `N.a`, `K?.a ?? d`):
    /// when it is (a part of) a module constant evaluated at each use, the constant is kept in
    /// the module state, or it is the error of `const v = K` (`moved_out_of_const`). A bare `K`
    /// is `global_read`'s.
    pub(crate) fn per_use_taken(&mut self, e: &ast::Expr, ty: TyId) {
        if self.copied_value(ty) {
            return;
        }
        if let Some((d, _, span)) = self.per_use_root(e, false, Parts::Take) {
            let part = crate::body::moved_const::ast_part(e);
            self.moved_out_of_const(d, ty, span, part);
        }
    }

    /// `wm.set(K!.a, 1)`, `new WeakRef(N.a)` (`h`: the key): a weak key is an object's
    /// identity, which a module constant evaluated at each use (or a part of one) has anew at
    /// each use, so the entry could never be found again: kept, or the error of `const v = K`.
    pub(super) fn per_use_weak_key(&mut self, h: &crate::hir::Expr) {
        use crate::body::moved_const as mc;
        if self.copied_value(h.ty) {
            return;
        }
        if let Some((d, part)) = mc::hir_root(self.cx, h) {
            self.moved_out_of_const(d, h.ty, h.span, part);
        }
    }

    /// `const { a } = K!` / `const [x] = N` (`pattern`, from `init`): a name bound to a part
    /// that is not copied (an object, an array) takes that part over, as `const a = K!.a`
    /// does. Checked after the declaration, from the types its names were given.
    pub(crate) fn per_use_destructured(
        &mut self,
        root: Option<(DefId, String, Span)>,
        pattern: &ast::Pattern,
    ) {
        let Some((d, _, span)) = root else { return };
        let mut names = Vec::new();
        pattern_names(pattern, &mut names);
        for name in names {
            let Some(l) = self.declared_local(name) else {
                continue;
            };
            let ty = self.f.locals[l.0 as usize].ty;
            if !self.copied_value(ty) {
                self.moved_out_of_const(d, ty, span, None);
                return;
            }
        }
    }

    /// `{ ...K!, v: 2 }` (`props`): a spread of a module constant evaluated at each use copies
    /// the fields that hold objects, where TypeScript shares them: when one of the fields is not
    /// copied (or the fields are not known), the constant is kept, or it is the error of
    /// `const v = K`.
    pub(super) fn per_use_spreads(&mut self, props: &[ast::ObjectProp]) {
        for p in props {
            let ast::ObjectProp::Spread(e) = p else {
                continue;
            };
            let Some((d, _, span)) = self.per_use_root(e, true, Parts::Take) else {
                continue;
            };
            let Some(ty) = self.trial_type(e) else {
                continue;
            };
            let ty = self.cx.ty.opt_payload(ty).unwrap_or(ty);
            let fields: Vec<TyId> = match self.adt_of(ty).and_then(|(a, _)| self.cx.adt(a)) {
                Some(a) => a.fields.iter().map(|f| f.ty).collect(),
                None => vec![ty],
            };
            if let Some(t) = fields.into_iter().find(|&t| !self.copied_value(t)) {
                self.moved_out_of_const(d, t, span, None);
            }
        }
    }

    /// Is a value of type `ty` one that taking from a module constant changes nothing
    /// TypeScript would share: copied (a number, a string, either or `null`), or a function,
    /// which has nothing to write?
    pub(crate) fn copied_value(&mut self, ty: TyId) -> bool {
        crate::body::moved_const::copied_value(self.cx, ty)
    }

    /// The module constant (and its name and span) evaluated at each use that a destructuring
    /// declaration's initializer `init` gives, directly or through `!`, `as`, `??` or a part.
    pub(crate) fn per_use_init_root(&mut self, init: &ast::Expr) -> Option<(DefId, String, Span)> {
        self.per_use_root(init, true, Parts::Take)
    }

    /// The module constant evaluated at each use that `e` gives (a part of, with
    /// [`Parts::Take`]), when it is reached through a form that hides it from the place check
    /// (`through`).
    fn per_use_root(
        &mut self,
        e: &ast::Expr,
        through: bool,
        parts: Parts,
    ) -> Option<(DefId, String, Span)> {
        let e = strip_parens(e);
        match &e.kind {
            ast::ExprKind::NonNull(x) | ast::ExprKind::Cast { expr: x, .. } => {
                self.per_use_root(x, true, parts)
            }
            ast::ExprKind::Member { object, prop, .. } => {
                if let Some(id) = self.namespace_member(object, prop) {
                    let d = self.module_global(&id.name)?;
                    return self.per_use_global(d, through, e.span);
                }
                if let Some(d) = self.static_global(object, prop) {
                    return self.per_use_global(d, through, e.span);
                }
                self.per_use_root(object, through || parts == Parts::Take, parts)
            }
            ast::ExprKind::Index { object, .. } => {
                self.per_use_root(object, through || parts == Parts::Take, parts)
            }
            ast::ExprKind::Binary { op, lhs, rhs }
                if matches!(
                    op,
                    ast::BinaryOp::Nullish | ast::BinaryOp::Or | ast::BinaryOp::And
                ) =>
            {
                let l = match op {
                    ast::BinaryOp::And => None,
                    _ => self.per_use_root(lhs, true, parts),
                };
                l.or_else(|| self.per_use_root(rhs, true, parts))
            }
            ast::ExprKind::Cond { then, els, .. } => self
                .per_use_root(then, true, parts)
                .or_else(|| self.per_use_root(els, true, parts)),
            ast::ExprKind::Ident(id) if !self.is_local_name(&id.name) => {
                let d = self.module_global(&id.name)?;
                self.per_use_global(d, through, e.span)
            }
            _ => None,
        }
    }

    /// The module-level variable `name` (`K`, `ns.K`) names.
    fn module_global(&mut self, name: &str) -> Option<DefId> {
        let Some(crate::ctx::Item::Def(d)) = self.cx.lookup_item(self.module, name) else {
            return None;
        };
        matches!(self.cx.info[d.0 as usize], crate::defs::DefInfo::Global(_)).then_some(d)
    }

    /// The `static readonly` field `Cls.prop` (`object`: `Cls`, or `ns.Cls` through a
    /// namespace import) of a class or of a base class.
    fn static_global(&mut self, object: &ast::Expr, prop: &ast::Ident) -> Option<DefId> {
        let id = match &strip_parens(object).kind {
            ast::ExprKind::Ident(id) if !self.is_local_name(&id.name) => id.clone(),
            ast::ExprKind::Member {
                object: ns,
                prop: cls,
                optional: false,
            } => self.namespace_member(ns, cls)?,
            _ => return None,
        };
        let item = self.cx.lookup_item_at(self.module, &id.name, id.span);
        let Some(crate::ctx::Item::Def(mut cur)) = item else {
            return None;
        };
        for _ in 0..64 {
            if let Some(&g) = self.cx.adt(cur)?.statics.get(&prop.name) {
                return Some(g);
            }
            let base = self.cx.adt(cur)?.base?;
            cur = self.cx.class_of(base)?.0;
        }
        None
    }

    /// Module-level variable `d`, reached `through` a hiding form, when it is a constant
    /// evaluated at each use whose value is not copied.
    fn per_use_global(
        &mut self,
        d: DefId,
        through: bool,
        span: Span,
    ) -> Option<(DefId, String, Span)> {
        if !through {
            return None;
        }
        crate::body::driver::ensure_global(self.cx, d);
        let (stored, ty, name) = self
            .cx
            .global(d)
            .map(|g| (g.stored, g.ty, g.name.clone()))?;
        (!stored && !self.cx.is_copy(ty)).then_some((d, name, span))
    }
}

/// The names `p` binds.
fn pattern_names<'p>(p: &'p ast::Pattern, out: &mut Vec<&'p str>) {
    match &p.kind {
        ast::PatternKind::Ident(id) => out.push(&id.name),
        ast::PatternKind::Wildcard => {}
        ast::PatternKind::Object { fields, rest } => {
            fields.iter().for_each(|(_, q)| pattern_names(q, out));
            out.extend(rest.as_ref().map(|r| r.name.as_str()));
        }
        ast::PatternKind::Array { elems, rest } => {
            elems.iter().for_each(|q| pattern_names(q, out));
            out.extend(rest.as_ref().map(|r| r.name.as_str()));
        }
        ast::PatternKind::Default { pattern, .. } => pattern_names(pattern, out),
    }
}
