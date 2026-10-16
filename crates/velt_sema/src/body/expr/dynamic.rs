//! Conversions to and from `any` / `unknown` (crate::any; hir_encodings.md "any and
//! unknown"). Every value converts to either (`ToDyn` with `Program::any_impl`, after
//! unpacking `T | null`, unions and literal types, so the box holds one concrete type). An
//! `any` converts to every type, checked at run time (`Intrinsic::AnyTo`); an `unknown` only
//! to `any`, or with `as T`.

use velt_common::{Diagnostic, Span};

use super::union_coerce::outer_mode;
use crate::any::Dynamic;
use crate::body::places::{is_place, set_place_mode};
use crate::body::{FnCx, LocalKind};
use crate::hir::{
    self, ExprKind as H, Intrinsic, LocalId, Nothing, PatKind as P, TyId, TyKind, UseMode,
};

/// Does a value of the dynamic type `from` convert to the dynamic type `to`? Every one converts
/// to `any` and `unknown`, an `any` and a `Function` to `object`, an `any` to `Function`; the
/// others only with `as` (`checked`), as in TypeScript. They share one representation.
fn dynamic_converts(from: Dynamic, to: Dynamic, checked: bool) -> bool {
    checked
        || from == to
        || matches!(
            (from, to),
            (_, Dynamic::Any | Dynamic::Unknown)
                | (Dynamic::Any, _)
                | (Dynamic::Function, Dynamic::Object)
        )
}

impl FnCx<'_, '_> {
    /// `h` converted to `exp`, one of them `any` or `unknown`. `checked_from_unknown`: an
    /// `unknown` may convert to a typed value (`as T`).
    pub(crate) fn coerce_dynamic(
        &mut self,
        h: hir::Expr,
        exp: TyId,
        checked_from_unknown: bool,
    ) -> Result<hir::Expr, hir::Expr> {
        let to = self.cx.dynamic(exp);
        let from = self.cx.dynamic(h.ty);
        match (from, to) {
            (None, None) => Err(h),
            (None, Some(d)) if !self.dynamic_takes_static(d, h.ty) => Err(h),
            (None, Some(_)) => self.box_dynamic(h, exp),
            (Some(f), Some(t)) if !dynamic_converts(f, t, checked_from_unknown) => Err(h),
            (Some(Dynamic::Any), _) | (Some(_), Some(_)) => Ok(self.unbox_dynamic(h, exp)),
            (Some(_), None) if !checked_from_unknown => Err(h),
            (Some(_), None) => Ok(self.unbox_dynamic(h, exp)),
        }
    }

    /// Does the dynamic type `d` take a value of the static type `t`? `object` takes
    /// non-primitive types (a type parameter through its bound, `T extends object`), `Function`
    /// function types; `any` and `unknown` take every type.
    fn dynamic_takes_static(&mut self, d: Dynamic, t: TyId) -> bool {
        let Some(defs) = self.cx.any else {
            return false;
        };
        let iface = match d {
            Dynamic::Any | Dynamic::Unknown => return true,
            Dynamic::Object => defs.object,
            Dynamic::Function => defs.function,
        };
        if let TyKind::Param(_) = self.cx.ty.kind(t) {
            let b = crate::defs::Bound {
                iface,
                args: vec![],
            };
            let bounds = self.bounds.clone();
            return self.cx.satisfies(t, &b, &bounds);
        }
        self.cx.dynamic_takes(iface, t).unwrap_or(false)
    }

    /// `h as object` for an `h` whose type is a type parameter without an `object` bound, or
    /// one or `null` (sigx's `toRaw<T>`, issue #926): lowering unpacks the instantiation (a
    /// `T | null`, a union) at run time, and a primitive is a `TypeError` there. `Err` when `h`
    /// is no such value or `exp` is not `object`.
    pub(crate) fn param_as_object(
        &mut self,
        h: hir::Expr,
        exp: TyId,
    ) -> Result<hir::Expr, hir::Expr> {
        if self.cx.dynamic(exp) != Some(Dynamic::Object) {
            return Err(h);
        }
        let inner = self.cx.ty.opt_payload(h.ty).unwrap_or(h.ty);
        if !matches!(self.cx.ty.kind(inner), TyKind::Param(_)) {
            return Err(h);
        }
        self.box_dynamic(h, exp)
    }

    /// A typed value boxed as `exp` (`any` or `unknown`).
    fn box_dynamic(&mut self, h: hir::Expr, exp: TyId) -> Result<hir::Expr, hir::Expr> {
        let span = h.span;
        // Already dynamic (the payload of an `any | null`): the same value, never a box in a
        // box.
        if self.cx.dynamic(h.ty).is_some() {
            let mut h = h;
            if is_place(&h) && outer_mode(&h) == Some(UseMode::Borrow) {
                // A borrowed payload: a share of it.
                let ty = h.ty;
                h = self.intrinsic(Intrinsic::Share, vec![h], ty, span);
            } else {
                self.force_move(&mut h);
            }
            return Ok(if h.ty == exp {
                h
            } else {
                self.unbox_dynamic(h, exp)
            });
        }
        if let Some(v) = self.cx.lit_value(h.ty) {
            let b = self.lit_to_base(h, &v);
            return self.box_dynamic(b, exp);
        }
        if self.cx.union_def(h.ty).is_some() {
            return self.union_to_common(h, exp);
        }
        if let Some(def) = self.plain_enum(h.ty) {
            return Ok(self.enum_to_dynamic(h, def, exp));
        }
        if let Some(payload) = self.cx.ty.opt_payload(h.ty) {
            if !matches!(h.kind, H::Lit(hir::Lit::Null)) {
                let nothing = self.cx.ty.nothing_of(h.ty).unwrap_or(Nothing::Null);
                return self.option_to_dynamic(h, payload, exp, nothing);
            }
        }
        let narrow = matches!(
            self.cx.dynamic(exp),
            Some(Dynamic::Object | Dynamic::Function)
        );
        match self.cx.ty.kind(h.ty) {
            // A type parameter bound by `object` / `Function` holds no `null` and no primitive
            // to unpack (issue #926); lowering boxes the instantiation's value.
            TyKind::Param(_) if narrow => {}
            TyKind::Unit | TyKind::Never | TyKind::Error | TyKind::Param(_) => return Err(h),
            TyKind::Promise(..) => return Err(h),
            _ => {}
        }
        let Some(imp) = self.cx.any_impl() else {
            return Err(h);
        };
        let mut h = h;
        self.force_move(&mut h);
        Ok(self.mk(
            H::ToDyn {
                expr: Box::new(h),
                impl_index: imp,
            },
            exp,
            span,
        ))
    }

    /// `h` as an `any` (`unknown`) when it is an `any | null` (`unknown | null`): the value,
    /// or the dynamic `null` / `undefined` for the nothing (TypeScript has no `any | null`).
    pub(crate) fn collapse_dynamic_option(&mut self, mut h: hir::Expr) -> hir::Expr {
        let Some(payload) = self.cx.ty.opt_payload(h.ty) else {
            return h;
        };
        if self.cx.dynamic(payload).is_none() {
            return h;
        }
        // The standard library's `T | null` results are JavaScript's `undefined` when absent
        // (`Map.get`, `find`, `pop`, a `Record` index).
        let nothing = if self.std_result(&h) {
            Nothing::Undefined
        } else {
            self.cx.ty.nothing_of(h.ty).unwrap_or(Nothing::Null)
        };
        if is_place(&h) {
            // A field or variable keeps its value: the result is a share of it.
            set_place_mode(&mut h, UseMode::Borrow);
        }
        match self.option_to_dynamic(h, payload, payload, nothing) {
            Ok(h) | Err(h) => h,
        }
    }

    /// `s ?? rhs` on an `any` / `unknown` `s`: `rhs` (boxed) when `s` holds `null` or
    /// `undefined`, else `s`.
    pub(crate) fn dynamic_nullish(
        &mut self,
        s: hir::Expr,
        rhs: &velt_syntax::ast::Expr,
        span: Span,
    ) -> hir::Expr {
        let dty = s.ty;
        let init = self.share_if_place(s);
        let tmp = self.new_local("<nullish>", dty, false, span, LocalKind::Temp);
        let read = self.mk(H::Local(tmp, UseMode::Borrow), dty, span);
        let nil = self.dynamic_null_compare(read, "null", false, true, span);
        let d = self.expr(rhs, Some(dty), crate::body::Want::Move);
        let d = self.coerce(d, dty);
        let value = self.mk(H::Local(tmp, UseMode::Move), dty, span);
        let kind = H::If {
            cond: Box::new(nil),
            then: Box::new(d),
            els: Box::new(value),
        };
        let result = self.mk(kind, dty, span);
        let block = hir::Block {
            stmts: vec![hir::Stmt {
                kind: hir::StmtKind::Let {
                    local: tmp,
                    init: Some(init),
                },
                span,
            }],
            value: Some(Box::new(result)),
            span,
        };
        self.mk(H::Block(block), dty, span)
    }

    /// `null` or `undefined` as a dynamic value of type `exp`.
    pub(crate) fn dynamic_nothing(&mut self, n: Nothing, exp: TyId, span: Span) -> hir::Expr {
        let t = self.cx.dynamic_nothing_ty(n);
        let null = self.mk(H::Lit(hir::Lit::Null), t, span);
        match self.box_dynamic(null, exp) {
            Ok(h) | Err(h) => h,
        }
    }

    /// `T | null` boxed: its payload, or the dynamic `null` / `undefined`.
    fn option_to_dynamic(
        &mut self,
        h: hir::Expr,
        payload: TyId,
        exp: TyId,
        nothing: Nothing,
    ) -> Result<hir::Expr, hir::Expr> {
        let span = h.span;
        let consume = !is_place(&h) || outer_mode(&h) == Some(UseMode::Move);
        let mode = if self.cx.is_copy(payload) {
            UseMode::Copy
        } else if consume {
            UseMode::Move
        } else {
            UseMode::Borrow
        };
        let b = self.new_local("<some>", payload, false, span, LocalKind::Bind);
        let value = self.mk(H::Local(b, mode), payload, span);
        let boxed = self.box_dynamic(value, exp)?;
        let none = self.dynamic_nothing(nothing, exp, span);
        let bind = hir::Pat {
            kind: P::Binding(b, mode),
            ty: payload,
            span,
        };
        let arms = vec![
            hir::Arm {
                pat: hir::Pat {
                    kind: P::Some(Box::new(bind)),
                    ty: h.ty,
                    span,
                },
                guard: None,
                body: boxed,
            },
            hir::Arm {
                pat: hir::Pat {
                    kind: P::None,
                    ty: h.ty,
                    span,
                },
                guard: None,
                body: none,
            },
        ];
        let m = H::Match {
            scrutinee: Box::new(h),
            arms,
        };
        Ok(self.mk(m, exp, span))
    }

    /// `h as target` on a dynamic `h`: checked at run time.
    pub(crate) fn cast_dynamic(&mut self, h: hir::Expr, target: TyId) -> hir::Expr {
        match self.coerce_dynamic(h, target, true) {
            Ok(h) | Err(h) => h,
        }
    }

    /// A dynamic value as `exp`: checked at run time (`Intrinsic::AnyTo`).
    fn unbox_dynamic(&mut self, h: hir::Expr, exp: TyId) -> hir::Expr {
        let span = h.span;
        let members = match self.cx.union_members(exp) {
            Some(ms) => Some(ms),
            None if self.cx.lit_value(exp).is_some() => Some(vec![exp]),
            None => None,
        };
        if let Some(members) = members {
            return self.dynamic_to_members(h, exp, members);
        }
        if let Some(def) = self.plain_enum(exp) {
            return self.dynamic_to_enum(h, def, exp);
        }
        if let Some(payload) = self.cx.ty.opt_payload(exp) {
            if self.cx.union_def(payload).is_some()
                || self.cx.lit_value(payload).is_some()
                || self.plain_enum(payload).is_some()
            {
                return self.dynamic_to_optional(h, exp, payload);
            }
        }
        let h = self.share_if_place(h);
        self.intrinsic(Intrinsic::AnyTo, vec![h], exp, span)
    }

    /// A variable keeps its value: a conversion out of it takes a share of it, reading the
    /// variable (which stays owned and is dropped with its scope).
    pub(crate) fn share_if_place(&mut self, mut h: hir::Expr) -> hir::Expr {
        if !is_place(&h) {
            return h;
        }
        let (ty, span) = (h.ty, h.span);
        set_place_mode(&mut h, UseMode::Borrow);
        self.intrinsic(Intrinsic::Share, vec![h], ty, span)
    }

    /// A dynamic value as `exp`, a union or literal type `payload` that also allows `null` /
    /// `undefined`: `null` and `undefined` give the nothing, anything else is converted to
    /// `payload`.
    fn dynamic_to_optional(&mut self, h: hir::Expr, exp: TyId, payload: TyId) -> hir::Expr {
        let span = h.span;
        let dty = h.ty;
        let init = self.share_if_place(h);
        let tmp = self.new_local("<any>", dty, false, span, LocalKind::Temp);
        let read = self.mk(H::Local(tmp, UseMode::Borrow), dty, span);
        let nil = self.dynamic_null_compare(read, "null", false, true, span);
        let none = self.mk(H::Lit(hir::Lit::Null), exp, span);
        let read = self.mk(H::Local(tmp, UseMode::Borrow), dty, span);
        let value = self.unbox_dynamic(read, payload);
        let some = self.coerce(value, exp);
        let kind = H::If {
            cond: Box::new(nil),
            then: Box::new(none),
            els: Box::new(some),
        };
        let result = self.mk(kind, exp, span);
        let block = hir::Block {
            stmts: vec![hir::Stmt {
                kind: hir::StmtKind::Let {
                    local: tmp,
                    init: Some(init),
                },
                span,
            }],
            value: Some(Box::new(result)),
            span,
        };
        self.mk(H::Block(block), exp, span)
    }

    /// A dynamic value as the union or literal type `exp` (`members`: its members, or `exp`
    /// itself): the first member whose test the value passes (`typeof` for a primitive,
    /// `instanceof` for a class, `===` for a literal), converted; else a `TypeError`.
    fn dynamic_to_members(&mut self, h: hir::Expr, exp: TyId, members: Vec<TyId>) -> hir::Expr {
        let span = h.span;
        let dty = h.ty;
        let init = self.share_if_place(h);
        let tmp = self.new_local("<any>", dty, false, span, LocalKind::Temp);
        let name = self.cx.display(exp);
        let msg = self.str_lit(&format!("TypeError: the value is not a {name}"), span);
        let never = self.cx.ty.never;
        let mut result = self.intrinsic(Intrinsic::Panic, vec![msg], never, span);
        for &m in members.iter().rev() {
            let Some((test, value)) = self.member_test(tmp, dty, m, span) else {
                let d = Diagnostic::error(
                    format!(
                        "converting an `any` to `{name}` is not supported yet (#924): no run-time test for `{}`",
                        self.cx.display(m)
                    ),
                    span,
                )
                .with_note("test the value first (`typeof`, `instanceof`) and convert it to that member");
                self.cx.error(d);
                return self.error_expr(span);
            };
            let value = self.coerce(value, exp);
            let kind = H::If {
                cond: Box::new(test),
                then: Box::new(value),
                els: Box::new(result),
            };
            result = self.mk(kind, exp, span);
        }
        let block = hir::Block {
            stmts: vec![hir::Stmt {
                kind: hir::StmtKind::Let {
                    local: tmp,
                    init: Some(init),
                },
                span,
            }],
            value: Some(Box::new(result)),
            span,
        };
        self.mk(H::Block(block), exp, span)
    }

    /// The test that the dynamic value in `tmp` (type `dty`) holds a `m`, and the value as `m`.
    fn member_test(
        &mut self,
        tmp: LocalId,
        dty: TyId,
        m: TyId,
        span: Span,
    ) -> Option<(hir::Expr, hir::Expr)> {
        let b = self.cx.ty.bool_;
        let read = |s: &mut Self| s.mk(H::Local(tmp, UseMode::Borrow), dty, span);
        if let Some(v) = self.cx.lit_value(m) {
            let base = self.base_lit_expr(&v, span);
            let any = self.any_ty()?;
            let boxed = self.coerce(base, any);
            let x = read(self);
            let test = self.intrinsic(Intrinsic::AnyStrictEq, vec![x, boxed], b, span);
            return Some((test, self.lit_const(m, span)));
        }
        let test = match self.cx.ty.kind(m) {
            TyKind::Int(_) | TyKind::Float(_) | TyKind::Str | TyKind::Bool => {
                let tag = self.cx.typeof_tag(m);
                let str_ = self.cx.ty.str_;
                let x = read(self);
                let t = self.intrinsic(Intrinsic::AnyTypeof, vec![x], str_, span);
                let want = self.str_lit(tag, span);
                let kind = H::Binary {
                    op: hir::BinOp::Eq,
                    lhs: Box::new(t),
                    rhs: Box::new(want),
                };
                self.mk(kind, b, span)
            }
            _ => {
                let (class, _) = self.cx.class_of(m)?;
                let arms = vec![
                    hir::Arm {
                        pat: self.pat(P::InstanceOf(class), dty, span),
                        guard: None,
                        body: self.mk(H::Lit(hir::Lit::Bool(true)), b, span),
                    },
                    hir::Arm {
                        pat: self.pat(P::Wildcard, dty, span),
                        guard: None,
                        body: self.mk(H::Lit(hir::Lit::Bool(false)), b, span),
                    },
                ];
                let kind = H::Match {
                    scrutinee: Box::new(read(self)),
                    arms,
                };
                self.mk(kind, b, span)
            }
        };
        let x = read(self);
        let share = self.intrinsic(Intrinsic::Share, vec![x], dty, span);
        let value = self.intrinsic(Intrinsic::AnyTo, vec![share], m, span);
        Some((test, value))
    }

    /// The error for an `unknown` used where a typed value is needed.
    pub(crate) fn unknown_use_error(&mut self, span: Span) {
        self.cx.error(
            Diagnostic::error("an `unknown` value must be narrowed before use", span)
                .with_note("test it with `typeof`, `instanceof` or `=== null` first, or convert it with `as T` (checked at run time)"),
        );
    }
}

impl FnCx<'_, '_> {
    /// The type `any` (the prelude's interface); `None` without a prelude.
    pub(crate) fn any_ty(&mut self) -> Option<TyId> {
        let defs = self.cx.any?;
        Some(self.cx.ty.intern(TyKind::Dyn(defs.any, vec![])))
    }

    /// The variable of this function `e` names, when its type is `any` or `unknown`.
    pub(crate) fn own_dynamic_local(&mut self, e: &velt_syntax::ast::Expr) -> Option<LocalId> {
        use velt_syntax::ast::ExprKind as A;
        let name = match &e.kind {
            A::Paren(x) => return self.own_dynamic_local(x),
            A::Ident(i) => i.name.as_str(),
            _ => return None,
        };
        let l = self
            .f
            .scopes
            .iter()
            .rev()
            .find_map(|s| s.names.get(name).copied())?;
        self.cx.dynamic(self.local_ty(l)).map(|_| l)
    }

    /// `s === null` / `s == undefined` (`word`) on a dynamic value `s`.
    pub(crate) fn dynamic_null_compare(
        &mut self,
        s: hir::Expr,
        word: &str,
        strict: bool,
        is_eq: bool,
        span: Span,
    ) -> hir::Expr {
        let Some(any) = self.any_ty() else {
            return self.error_expr(span);
        };
        let n = if word == "undefined" {
            Nothing::Undefined
        } else {
            Nothing::Null
        };
        let l = self.dynamic_operand(s, any);
        let r = self.dynamic_nothing(n, any, span);
        let i = if strict {
            Intrinsic::AnyStrictEq
        } else {
            Intrinsic::AnyLooseEq
        };
        let b = self.cx.ty.bool_;
        let test = self.intrinsic(i, vec![l, r], b, span);
        self.negated(test, !is_eq, span)
    }

    /// An operand of a dynamic comparison: an `any` or `unknown` as it is (the comparison
    /// only reads it), anything else boxed.
    fn dynamic_operand(&mut self, h: hir::Expr, any: TyId) -> hir::Expr {
        if self.cx.dynamic(h.ty).is_some() {
            h
        } else {
            self.coerce(h, any)
        }
    }

    /// `l === r` / `l == r` (`negate`: `!==` / `!=`) where the checked type of an operand is
    /// `any` or `unknown`: both sides boxed (`Intrinsic::AnyStrictEq` / `AnyLooseEq`).
    pub(crate) fn dynamic_eq(
        &mut self,
        l: hir::Expr,
        r: hir::Expr,
        strict: bool,
        negate: bool,
        span: Span,
    ) -> hir::Expr {
        let Some(any) = self.any_ty() else {
            return self.error_expr(span);
        };
        let l = self.dynamic_operand(l, any);
        let r = self.dynamic_operand(r, any);
        let i = if strict {
            Intrinsic::AnyStrictEq
        } else {
            Intrinsic::AnyLooseEq
        };
        let b = self.cx.ty.bool_;
        let test = self.intrinsic(i, vec![l, r], b, span);
        self.negated(test, negate, span)
    }
}

impl FnCx<'_, '_> {
    /// An operator other than `==` / `===` with an `any` or `unknown` operand (phase 2 of
    /// #924 for `any`; `unknown` must be narrowed first).
    pub(crate) fn dynamic_operator(
        &mut self,
        op: &str,
        l: &hir::Expr,
        r: &hir::Expr,
        span: Span,
    ) -> hir::Expr {
        if [l.ty, r.ty]
            .iter()
            .any(|t| self.cx.dynamic(*t) == Some(Dynamic::Unknown))
        {
            self.unknown_use_error(span);
        } else {
            self.cx.error(
                Diagnostic::error(
                    format!("the operator `{op}` on an `any` value is not supported yet (#924)"),
                    span,
                )
                .with_note("convert the value to a typed one first: `(x as number) + 1`"),
            );
        }
        self.error_expr(span)
    }

    /// The enum of `t` when it is a declared enum without payloads (`enum Color { Red }`), whose
    /// values are numbers or strings in JavaScript.
    fn plain_enum(&self, t: TyId) -> Option<hir::DefId> {
        if self.cx.union_def(t).is_some() || !self.is_c_like_enum(t) {
            return None;
        }
        match self.cx.ty.kind(t) {
            TyKind::Adt(d, _) => Some(*d),
            _ => None,
        }
    }

    /// The JavaScript value of each member of the enum `def`: its number or its string.
    fn enum_member_values(&mut self, def: hir::DefId, span: Span) -> Vec<hir::Expr> {
        let values: Vec<Result<String, i64>> = self
            .cx
            .enum_info(def)
            .map(|e| {
                e.variants
                    .iter()
                    .map(|v| v.str_value.clone().ok_or(v.discriminant))
                    .collect()
            })
            .unwrap_or_default();
        values
            .into_iter()
            .map(|v| match v {
                Ok(s) => self.str_lit(&s, span),
                Err(n) => self.mk(H::Lit(hir::Lit::Float(n as f64)), self.cx.ty.f64, span),
            })
            .collect()
    }

    /// An enum value boxed as its number or string, as JavaScript holds it.
    fn enum_to_dynamic(&mut self, h: hir::Expr, def: hir::DefId, exp: TyId) -> hir::Expr {
        let (span, ety) = (h.span, h.ty);
        let values = self.enum_member_values(def, span);
        let arms = values
            .into_iter()
            .enumerate()
            .map(|(i, v)| hir::Arm {
                pat: self.pat(
                    P::Variant {
                        def,
                        variant: i as u32,
                        args: vec![],
                    },
                    ety,
                    span,
                ),
                guard: None,
                body: self.coerce(v, exp),
            })
            .collect();
        let kind = H::Match {
            scrutinee: Box::new(h),
            arms,
        };
        self.mk(kind, exp, span)
    }

    /// A dynamic value as the enum `exp`: the member whose number or string it is (`===`), or a
    /// `TypeError`.
    fn dynamic_to_enum(&mut self, h: hir::Expr, def: hir::DefId, exp: TyId) -> hir::Expr {
        let span = h.span;
        let dty = h.ty;
        let Some(any) = self.any_ty() else {
            return self.error_expr(span);
        };
        let type_args = match self.cx.ty.kind(exp) {
            TyKind::Adt(_, a) => a.clone(),
            _ => vec![],
        };
        let init = self.share_if_place(h);
        let tmp = self.new_local("<any>", dty, false, span, LocalKind::Temp);
        let name = self.cx.display(exp);
        let msg = self.str_lit(&format!("TypeError: the value is not a {name}"), span);
        let never = self.cx.ty.never;
        let b = self.cx.ty.bool_;
        let mut result = self.intrinsic(Intrinsic::Panic, vec![msg], never, span);
        let values = self.enum_member_values(def, span);
        for (i, v) in values.into_iter().enumerate().rev() {
            let boxed = self.coerce(v, any);
            let x = self.mk(H::Local(tmp, UseMode::Borrow), dty, span);
            let test = self.intrinsic(Intrinsic::AnyStrictEq, vec![x, boxed], b, span);
            let member = self.mk(
                H::Variant {
                    def,
                    type_args: type_args.clone(),
                    variant: i as u32,
                    args: vec![],
                },
                exp,
                span,
            );
            let kind = H::If {
                cond: Box::new(test),
                then: Box::new(member),
                els: Box::new(result),
            };
            result = self.mk(kind, exp, span);
        }
        let block = hir::Block {
            stmts: vec![hir::Stmt {
                kind: hir::StmtKind::Let {
                    local: tmp,
                    init: Some(init),
                },
                span,
            }],
            value: Some(Box::new(result)),
            span,
        };
        self.mk(H::Block(block), exp, span)
    }
}
