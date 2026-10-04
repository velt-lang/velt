//! Widening a fresh container (#268): `C[]` → `Named[]`, `(i64)[]` → `(i64 | null)[]`,
//! `Box<C>` → `Box<Named>` when the value is a call result, a `new` expression or a literal.
//! TypeScript converts any such value (arrays are covariant there), which is unsound for writes:
//! a `Named` pushed through the `Named[]` would land in the `C[]`. Velt converts only values
//! nothing else holds yet, and builds the wider value instead of reinterpreting it (an interface
//! value or a union is laid out differently from a `C`):
//! - an array is consumed into a new array, each element converted;
//! - an object type or a generic class without base class, subclasses or drop hook becomes a new
//!   object whose fields are shares of the old one's, each converted.
//!
//! Anything else (a variable, a field) stays an error with a fix: copy it (`[...xs]`, `map`).
//! The conversion costs one pass over the value, paid only where a program writes it.

use velt_common::Diagnostic;

use crate::body::{FnCx, LocalKind};
use crate::fresh_returns::{fresh_callees, FreshCheck};
use crate::hir::{
    self, AdtKind, ExprKind as H, Intrinsic, Pat, PatKind, StmtKind as S, TyId, TyKind, UseMode,
};

/// Is `h` a value nothing else references yet (a call result, `new`, a literal)? A call's
/// callee is checked once every body is (`crate::fresh_returns`).
pub(crate) fn is_fresh(h: &hir::Expr) -> bool {
    fresh_callees(h).is_some()
}

impl FnCx<'_, '_> {
    /// Does a single value of type `from` convert to `to` (as `try_coerce` converts it: to a
    /// nullable, a union, a base class, an interface it implements, a literal's base)?
    pub(crate) fn converts_to(&mut self, from: TyId, to: TyId) -> bool {
        if from == to || self.cx.ty.is_bottom(from) {
            return true;
        }
        if let Some(p) = self.cx.ty.opt_payload(to) {
            let from = self.cx.ty.opt_payload(from).unwrap_or(from);
            return self.converts_to(from, p);
        }
        if let Some(fs) = self.cx.union_members(from) {
            return fs.iter().all(|f| self.converts_to(*f, to));
        }
        if let Some(ms) = self.cx.union_members(to) {
            return ms.iter().any(|m| self.converts_to(from, *m));
        }
        if self.cx.lit_value(from).is_some() {
            let base = self.cx.widened(from);
            return base != from && self.converts_to(base, to);
        }
        if to == self.cx.ty.str_ && self.is_string_enum(from) {
            return true;
        }
        if let TyKind::Dyn(iface, args) = self.cx.ty.kind(to).clone() {
            return self
                .cx
                .find_impl(from, iface)
                .is_some_and(|(_, iargs, _)| iargs == args);
        }
        self.extends(from, to)
    }

    /// Is class type `from` a (transitive) subclass of `to`?
    fn extends(&mut self, from: TyId, to: TyId) -> bool {
        let mut cur = from;
        for _ in 0..64 {
            match self.cx.base_of(cur) {
                Some(b) if b == to => return true,
                Some(b) => cur = b,
                None => return false,
            }
        }
        false
    }

    /// Does a fresh value of type `from` widen to `to` (module docs)?
    pub(crate) fn widens(&mut self, from: TyId, to: TyId) -> bool {
        if from == to {
            return false;
        }
        let both = (self.cx.ty.kind(from).clone(), self.cx.ty.kind(to).clone());
        match both {
            (TyKind::Array(a), TyKind::Array(b)) => self.converts_to(a, b) || self.widens(a, b),
            (TyKind::Adt(..), TyKind::Adt(..)) => self.widened_fields(from, to).is_some(),
            _ => false,
        }
    }

    /// `(field type in from, field type in to)` of two instances of one widenable object type.
    fn widened_fields(&mut self, from: TyId, to: TyId) -> Option<Vec<(TyId, TyId)>> {
        let (TyKind::Adt(d, xs), TyKind::Adt(e, ys)) =
            (self.cx.ty.kind(from).clone(), self.cx.ty.kind(to).clone())
        else {
            return None;
        };
        if d != e || xs == ys || self.cx.union_def(from).is_some() {
            return None;
        }
        let a = self.cx.adt(d)?;
        let class = a.kind == AdtKind::Class;
        if a.base.is_some() || a.has_dispose {
            return None;
        }
        let tys: Vec<TyId> = a.fields.iter().map(|f| f.ty).collect();
        if class && self.class_subclasses(from).is_some() {
            return None;
        }
        let mut out = vec![];
        for t in tys {
            let (f, g) = (self.cx.ty.subst(t, &xs), self.cx.ty.subst(t, &ys));
            if !self.converts_to(f, g) {
                return None;
            }
            out.push((f, g));
        }
        Some(out)
    }

    /// The fresh value `h` widened to `exp` (`Err` gives it back).
    pub(super) fn widen_fresh(&mut self, h: hir::Expr, exp: TyId) -> Result<hir::Expr, hir::Expr> {
        if !is_fresh(&h) || !self.widens(h.ty, exp) {
            return Err(h);
        }
        if self.detached {
            return Ok(self.detached_widening_error(&h, exp));
        }
        let callees = fresh_callees(&h).unwrap_or_default();
        if !callees.is_empty() {
            let (span, from) = (h.span, h.ty);
            self.cx.fresh_checks.push(FreshCheck {
                callees,
                span,
                from,
                to: exp,
            });
        }
        Ok(self.widen_owned(h, exp))
    }

    /// A fresh value widened where no body holds the conversion's temporaries (a field
    /// initializer or a default value): reported with the conversion to write instead.
    fn detached_widening_error(&mut self, h: &hir::Expr, exp: TyId) -> hir::Expr {
        let (e, f) = (self.cx.display(exp), self.cx.display(h.ty));
        let d = Diagnostic::error(
            format!("a `{f}` cannot be converted to `{e}` in a field initializer or a default value"),
            h.span,
        )
        .with_note("TypeScript allows this; Velt converts such a value by building a new one, which it does only inside a function body for now");
        let fix = match self.cx.ty.array_elem(exp) {
            Some(el) => format!(
                "convert the elements instead: `(…).map((x): {} => x)`",
                self.cx.display(el)
            ),
            None => "create the value in the constructor or the function body instead".into(),
        };
        let d = d.with_note(fix);
        self.cx.error(d);
        self.error_expr(h.span)
    }

    /// The owned value `h` (fresh, or an element moved out of one) converted to `exp`.
    fn convert_owned(&mut self, h: hir::Expr, exp: TyId) -> hir::Expr {
        if self.widens(h.ty, exp) {
            return self.widen_owned(h, exp);
        }
        self.coerce(h, exp)
    }

    fn widen_owned(&mut self, h: hir::Expr, exp: TyId) -> hir::Expr {
        match self.cx.ty.kind(exp).clone() {
            TyKind::Array(b) => self.widen_array(h, exp, b),
            _ => self.widen_object(h, exp),
        }
    }

    /// `{ let src = h; let out = with_capacity(src.length); for (e of src) out.push(<e>); out }`
    /// (a consuming loop unless the elements are Copy).
    fn widen_array(&mut self, h: hir::Expr, exp: TyId, to_elem: TyId) -> hir::Expr {
        let span = h.span;
        let from_elem = self
            .cx
            .ty
            .array_elem(h.ty)
            .expect("ICE: widen_array of a non-array");
        let mut lets = vec![];
        let src = self.let_temp("<widen>", h, true, &mut lets);
        let usize_ = self.cx.ty.usize;
        let src_read = self.mk(H::Local(src, UseMode::Borrow), self.local_ty(src), span);
        let len = self.intrinsic(Intrinsic::ArrayLen, vec![src_read], usize_, span);
        let init = self.intrinsic(Intrinsic::ArrayWithCapacity, vec![len], exp, span);
        let out = self.let_temp("<widened>", init, true, &mut lets);
        let copy = self.cx.is_copy(from_elem);
        let mode = if copy { UseMode::Copy } else { UseMode::Move };
        let e = self.new_local("<elem>", from_elem, false, span, LocalKind::Bind);
        let read = self.mk(H::Local(e, mode), from_elem, span);
        let value = self.convert_owned(read, to_elem);
        let target = self.mk(H::Local(out, UseMode::BorrowMut), exp, span);
        let unit = self.cx.ty.unit;
        let push = self.intrinsic(Intrinsic::ArrayPush, vec![target, value], unit, span);
        let src_mode = if copy { UseMode::Borrow } else { UseMode::Move };
        let iter = self.mk(H::Local(src, src_mode), self.local_ty(src), span);
        let body = hir::Block {
            stmts: vec![hir::Stmt {
                kind: S::Expr(push),
                span,
            }],
            value: None,
            span,
        };
        let binding = Pat {
            kind: PatKind::Binding(e, mode),
            ty: from_elem,
            span,
        };
        let kind = S::ForOf {
            label: None,
            binding,
            iter,
            body,
            consume: !copy,
        };
        lets.push(hir::Stmt { kind, span });
        let result = self.mk(H::Local(out, UseMode::Move), exp, span);
        self.with_lets(lets, result)
    }

    /// `{ let src = h; T { f: <share of src.f>, ... } }` for an object type or a generic class.
    fn widen_object(&mut self, h: hir::Expr, exp: TyId) -> hir::Expr {
        let span = h.span;
        let fields = self.widened_fields(h.ty, exp).expect("ICE: widen_object");
        let TyKind::Adt(def, type_args) = self.cx.ty.kind(exp).clone() else {
            panic!("ICE: widen_object of a non-object type");
        };
        let mut lets = vec![];
        let src = self.let_temp("<widen>", h, false, &mut lets);
        let src_ty = self.local_ty(src);
        let mut values = vec![];
        for (i, (from, to)) in fields.into_iter().enumerate() {
            let base = self.mk(H::Local(src, UseMode::Borrow), src_ty, span);
            let copy = self.cx.is_copy(from);
            let mode = if copy { UseMode::Copy } else { UseMode::Borrow };
            let kind = H::Field {
                base: Box::new(base),
                index: i as u32,
                mode,
            };
            let mut v = self.mk(kind, from, span);
            if !copy {
                v = self.intrinsic(Intrinsic::Share, vec![v], from, span);
            }
            values.push(self.coerce(v, to));
        }
        let lit = H::AdtLit {
            def,
            type_args,
            fields: values,
        };
        let lit = self.mk(lit, exp, span);
        self.with_lets(lets, lit)
    }

    /// `let <name> = init;` appended to `lets`.
    fn let_temp(
        &mut self,
        name: &str,
        init: hir::Expr,
        mutable: bool,
        lets: &mut Vec<hir::Stmt>,
    ) -> hir::LocalId {
        let (ty, span) = (init.ty, init.span);
        let l = self.new_local(name, ty, mutable, span, LocalKind::Temp);
        lets.push(hir::Stmt {
            kind: S::Let {
                local: l,
                init: Some(init),
            },
            span,
        });
        l
    }

    /// "mismatched types" for a value that would widen if it were fresh (module docs).
    pub(super) fn shared_widening_error(&mut self, expected: TyId, found: &hir::Expr) -> bool {
        if is_fresh(found) || !self.widens(found.ty, expected) {
            return false;
        }
        let (e, f) = (self.cx.display(expected), self.cx.display(found.ty));
        let what = self.value_name(found);
        let array = self.cx.ty.array_elem(expected).is_some();
        let wider = match self.widened_part(found.ty, expected) {
            Some((n, w)) => format!(
                "a `{}` that is not a `{}`",
                self.cx.display(w),
                self.cx.display(n)
            ),
            None => "another value".into(),
        };
        let d = Diagnostic::error(format!("cannot use {what} (`{f}`) as `{e}`"), found.span)
            .with_note(format!(
                "TypeScript allows this, but other code still sees {what} as `{f}`: storing {wider} into it through the `{e}` would break that"
            ))
            .with_note(self.widening_fix(found, expected, array));
        self.cx.error(d);
        true
    }

    /// The narrow and the wider type of a widening: `(C, Named)` for `C[]` → `Named[]`.
    fn widened_part(&mut self, from: TyId, to: TyId) -> Option<(TyId, TyId)> {
        match (self.cx.ty.kind(from).clone(), self.cx.ty.kind(to).clone()) {
            (TyKind::Array(a), TyKind::Array(b)) => self.widened_part(a, b).or(Some((a, b))),
            (TyKind::Adt(_, xs), TyKind::Adt(_, ys)) if self.cx.union_def(to).is_none() => {
                let (x, y) = xs.into_iter().zip(ys).find(|(x, y)| x != y)?;
                self.widened_part(x, y).or(Some((x, y)))
            }
            _ => None,
        }
    }

    /// "`xs`" / "`h.items`" for a place, "this value" otherwise.
    fn value_name(&self, h: &hir::Expr) -> String {
        match self.place_text(h) {
            Some(t) => format!("`{t}`"),
            None => "this value".into(),
        }
    }

    /// The source text of a local or a field path (`h.items`).
    fn place_text(&self, h: &hir::Expr) -> Option<String> {
        match &h.kind {
            H::Local(l, _) => Some(self.f.locals[l.0 as usize].name.clone()),
            H::Field { base, index, .. } => {
                let TyKind::Adt(d, _) = self.cx.ty.kind(base.ty) else {
                    return None;
                };
                let field = self.cx.adt(*d)?.fields.get(*index as usize)?;
                Some(format!("{}.{}", self.place_text(base)?, field.name))
            }
            _ => None,
        }
    }

    fn widening_fix(&mut self, found: &hir::Expr, expected: TyId, array: bool) -> String {
        let e = self.cx.display(expected);
        let name = self.place_text(found).unwrap_or_else(|| "xs".into());
        match self.cx.ty.array_elem(expected).filter(|_| array) {
            Some(el) => {
                let el = self.cx.display(el);
                format!(
                    "convert a copy instead: `[...{name}]` or `{name}.map((x): {el} => x)` (a new `{e}`)"
                )
            }
            None => format!("create a new `{e}` from its fields instead"),
        }
    }
}
