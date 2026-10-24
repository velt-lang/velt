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
        if from.is_some() && exp == self.cx.ty.never {
            return Ok(self.dynamic_to_never(h));
        }
        match (from, to) {
            (None, None) => Err(h),
            (None, Some(d)) if !self.dynamic_takes_static(d, h.ty) => Err(h),
            // A lock's value is read only under its lock, and what `any` holds is read (printed,
            // compared, written as JSON) without it.
            (None, Some(d)) if self.cx.holds_lock(h.ty) => {
                let (tn, to) = (self.cx.display(h.ty), d.name());
                self.cx.error(
                    velt_common::Diagnostic::error(
                        format!("cannot convert a `{tn}` to `{to}`: it holds a lock"),
                        h.span,
                    )
                    .with_note("its value is read only under its lock (`with`, `read`), and an `any` value is read without it: convert what a callback returns instead"),
                );
                Ok(self.error_expr(h.span))
            }
            (None, Some(_)) => self.box_dynamic(h, exp),
            (Some(f), Some(t)) if !dynamic_converts(f, t, checked_from_unknown) => Err(h),
            (Some(Dynamic::Any), _) | (Some(_), Some(_)) => Ok(self.unbox_dynamic(h, exp)),
            (Some(_), None) if self.takes_dynamic_whole(exp, h.ty) => {
                Ok(self.unbox_dynamic(h, exp))
            }
            (Some(_), None) if !checked_from_unknown => Err(h),
            (Some(_), None) => Ok(self.unbox_dynamic(h, exp)),
        }
    }

    /// An `any` / `unknown` where a `never` is expected (also a `bigint`, which is `never`,
    /// #1088): no value converts.
    fn dynamic_to_never(&mut self, h: hir::Expr) -> hir::Expr {
        let f = self.cx.display(h.ty);
        let mut d = Diagnostic::error(format!("an `{f}` value cannot be a `never`"), h.span)
            .with_note("no value has type `never`");
        if self.cx.ty.bigint_named {
            d = d.with_note(crate::types::BIGINT_NEVER_NOTE);
        }
        self.cx.error(d);
        self.error_expr(h.span)
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
        // `T & object` (or `T & …` at a `T extends object`) holds objects at every instance.
        if d == Dynamic::Object {
            if let TyKind::Op(crate::hir::TyOp::Intersect, _) = self.cx.ty.kind(t) {
                let objects = self.object_params();
                if self.cx.holds_objects(t, &objects) {
                    return true;
                }
            }
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

    /// Is `t` known only per instantiation: a type parameter, an intersection with one, or a
    /// union or `| null` with such a member (`T`, `T & object`, `T | string`)?
    pub(crate) fn per_instance(&mut self, t: TyId) -> bool {
        fn open(cx: &crate::ctx::Ctx, m: TyId, depth: u32) -> bool {
            match cx.ty.kind(m) {
                TyKind::Param(_) => true,
                TyKind::Op(hir::TyOp::Intersect, parts) if depth < 8 => {
                    parts.iter().any(|&p| open(cx, p, depth + 1))
                }
                // A deferred form over type parameters (`Partial<T>`, `Pick<T, K>`).
                TyKind::Op(hir::TyOp::Inline(_) | hir::TyOp::Alias(_), _) => cx.has_type_params(m),
                _ => false,
            }
        }
        let core = self.cx.ty.opt_payload(t).unwrap_or(t);
        open(self.cx, core, 0)
            || self
                .cx
                .union_members(core)
                .is_some_and(|ms| ms.iter().any(|&m| open(self.cx, m, 0)))
    }

    /// `h` shared into an `any` when its type is known only per instantiation
    /// ([`Self::per_instance`]), for what TypeScript allows on its apparent type that needs the
    /// value's run-time shape (`Object.keys`, `in`, `Array.isArray`, `x[k]`; issue #1212):
    /// lowering boxes each instantiation's value. `Err(h)` for any other `h`.
    pub(crate) fn generic_as_any(&mut self, h: hir::Expr) -> Result<hir::Expr, hir::Expr> {
        if !self.per_instance(h.ty) {
            return Err(h);
        }
        let Some(any) = self.any_ty() else {
            return Err(h);
        };
        let h = self.share_if_place(h);
        self.box_dynamic(h, any)
    }

    /// `x as D` where `x`'s type or `D` is known only per instantiation and nothing else
    /// converts it (`objectTarget as Signal<T & object>`, which asserts members the value may
    /// lack; issue #1304): the value through an `any`, checked at run time as `AnyTo` checks
    /// it, so an instantiation where the value already is a `D` keeps it, and any other stops
    /// with a `TypeError` naming both types.
    pub(crate) fn generic_assertion(
        &mut self,
        h: hir::Expr,
        target: TyId,
    ) -> Result<hir::Expr, hir::Expr> {
        // A primitive cast to a key type known only per instantiation (`key as SignalKey<T>`
        // with `key: string`) is checked the same way: the string must be one of the keys.
        let primitive = matches!(
            self.cx.ty.kind(h.ty),
            TyKind::Str | TyKind::Int(_) | TyKind::Float(_) | TyKind::Bool
        );
        if !((self.per_instance(h.ty) || primitive) && self.per_instance(target)) {
            return Err(h);
        }
        let span = h.span;
        let boxed = match primitive {
            true => {
                let Some(any) = self.any_ty() else {
                    return Err(h);
                };
                let h = self.share_if_place(h);
                self.box_dynamic(h, any)?
            }
            false => self.generic_as_any(h)?,
        };
        let boxed = self.share_if_place(boxed);
        Ok(self.intrinsic(Intrinsic::AnyTo, vec![boxed], target, span))
    }

    /// A key known only per instantiation (a value of `K extends SignalKey<T>`, of `keyof T`
    /// or another deferred form of string keys) where a `string` is expected: the key's
    /// string, through an `any`. Only for a form that gives strings at the type parameters'
    /// bounds. `Err(h)` for any other value.
    pub(crate) fn generic_key_string(
        &mut self,
        h: hir::Expr,
        exp: TyId,
    ) -> Result<hir::Expr, hir::Expr> {
        if exp != self.cx.ty.str_ {
            return Err(h);
        }
        let form = match self.cx.ty.kind(h.ty) {
            TyKind::Param(p) => match self.cx.constraint_of(*p) {
                Some(c) => c,
                None => return Err(h),
            },
            _ => h.ty,
        };
        if !matches!(
            self.cx.ty.kind(form),
            TyKind::Op(hir::TyOp::Alias(_) | hir::TyOp::Inline(_), _)
        ) {
            return Err(h);
        }
        // The form at the type parameters' object bounds (or `{}`): string keys?
        let n = self.bounds.len() as u32;
        let at_bounds: Vec<TyId> = (0..n)
            .map(|p| {
                self.cx
                    .constraint_of(p)
                    .filter(|&c| !self.cx.has_type_params(c))
                    .unwrap_or_else(|| self.cx.ty.param(p))
            })
            .collect();
        let at = self.cx.subst(form, &at_bounds);
        let str_ = self.cx.ty.str_;
        let strings = |cx: &mut crate::ctx::Ctx, t: TyId| {
            let ms = cx.union_members(t).unwrap_or_else(|| vec![t]);
            ms.iter().all(|&m| {
                m == str_ || matches!(cx.ty.kind(m), TyKind::Literal(hir::LitValue::Str(_)))
            })
        };
        if !strings(self.cx, at) && self.cx.has_type_params(at) {
            // Still deferred at the bounds (`keyof T` of an unbounded `T`): its keys are strings
            // whenever it is used as a key here.
        } else if !strings(self.cx, at) {
            return Err(h);
        }
        let Some(any) = self.any_ty() else {
            return Err(h);
        };
        let span = h.span;
        let h = self.share_if_place(h);
        let boxed = self.box_dynamic(h, any)?;
        Ok(self.intrinsic(Intrinsic::AnyTo, vec![boxed], str_, span))
    }

    /// The key of a weak operation (`WeakMap.set` / `get` / `has` / `delete`, `WeakSet`'s,
    /// `new WeakRef`) converted to its parameter `exp` (issue #926). A variable stays its own:
    /// the conversion to `object` takes a share of it, so the key (a fresh `WeakRef`'s target)
    /// lives to the end of the variable's scope, as a typed key does. An `any` enters `object`
    /// unchecked: a primitive reaches the operation, which finds nothing or throws
    /// JavaScript's `TypeError` (velt_vir `weak_object.rs`).
    pub(crate) fn weak_key_coerce(&mut self, h: hir::Expr, exp: TyId) -> hir::Expr {
        if self.cx.dynamic(exp) != Some(Dynamic::Object) {
            return self.coerce(h, exp);
        }
        match self.cx.dynamic(h.ty) {
            Some(Dynamic::Any) => {
                let Some(imp) = self.cx.any_impl() else {
                    return self.coerce(h, exp);
                };
                let mut h = self.share_if_place(h);
                self.force_move(&mut h);
                let span = h.span;
                self.mk(
                    H::ToDyn {
                        expr: Box::new(h),
                        impl_index: imp,
                    },
                    exp,
                    span,
                )
            }
            Some(_) => self.coerce(h, exp),
            None => {
                let h = self.share_place(h);
                self.coerce(h, exp)
            }
        }
    }

    /// A typed variable (a place) passed for a borrowed `object` parameter (issue #926): a
    /// share of it, so the variable lives to the end of its scope as it does when passed to a
    /// typed parameter (a `WeakRef` made from the parameter keeps its target).
    pub(crate) fn object_arg_coerce(&mut self, h: hir::Expr, exp: TyId) -> hir::Expr {
        if self.cx.dynamic(exp) != Some(Dynamic::Object) || self.cx.dynamic(h.ty).is_some() {
            return self.coerce(h, exp);
        }
        let h = self.share_place(h);
        self.coerce(h, exp)
    }

    fn share_place(&mut self, mut h: hir::Expr) -> hir::Expr {
        if is_place(&h) {
            set_place_mode(&mut h, UseMode::Borrow);
            let (ty, span) = (h.ty, h.span);
            h = self.intrinsic(Intrinsic::Share, vec![h], ty, span);
        }
        h
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
                // A `T | null | undefined`'s own nothing is `undefined` (its payload is the
                // `T | null`, boxed in turn).
                let nothing = match self.cx.ty.nothing_of(h.ty) {
                    Some(Nothing::Both) => Nothing::Undefined,
                    n => n.unwrap_or(Nothing::Null),
                };
                if payload == self.cx.ty.never {
                    // Of type `undefined` or `null` (`getU()` returning `undefined`).
                    return Ok(self.then_nothing(h, nothing, exp));
                }
                return self.option_to_dynamic(h, payload, exp, nothing);
            }
        }
        match self.cx.ty.kind(h.ty) {
            // A `void` value (a call of a function returning nothing) is `undefined`.
            TyKind::Unit => return Ok(self.then_nothing(h, Nothing::Undefined, exp)),
            // A type parameter bound by `object` / `Function` holds no `null` and no primitive
            // to unpack (issue #926); lowering boxes the instantiation's value. Into `any` or
            // `unknown`, lowering unpacks an instantiation that is an option, a union or a
            // literal type (issue #1241).
            TyKind::Param(_) => {}
            TyKind::Never | TyKind::Error => return Err(h),
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

    /// `{ h; <the dynamic nothing n> }`: `h` evaluated, then `n` as an `exp`.
    fn then_nothing(&mut self, h: hir::Expr, n: Nothing, exp: TyId) -> hir::Expr {
        let span = h.span;
        let none = self.dynamic_nothing(n, exp, span);
        let block = hir::Block {
            stmts: vec![hir::Stmt {
                kind: hir::StmtKind::Expr(h),
                span,
            }],
            value: Some(Box::new(none)),
            span,
        };
        self.mk(H::Block(block), exp, span)
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
        // A Velt-only standard library result that is `null` when absent (`Channel.receive` of
        // a closed channel) is `undefined` in an `any`, as JavaScript's "nothing there" is.
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
        // `undefined` (or `null`) alone as a type, an option of `never`: always its nothing
        // (a value typed `undefined` passed to an `any` parameter).
        if payload == self.cx.ty.never {
            let none = self.dynamic_nothing(nothing, exp, span);
            let block = hir::Block {
                stmts: vec![hir::Stmt {
                    kind: hir::StmtKind::Expr(h),
                    span,
                }],
                value: Some(Box::new(none)),
                span,
            };
            return Ok(self.mk(H::Block(block), exp, span));
        }
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
    pub(super) fn unbox_dynamic(&mut self, h: hir::Expr, exp: TyId) -> hir::Expr {
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
        if let (Some(payload), Some(core)) =
            (self.cx.ty.opt_payload(exp), self.present_payload(exp))
        {
            if self.cx.union_def(core).is_some()
                || self.cx.lit_value(core).is_some()
                || self.plain_enum(core).is_some()
            {
                return self.dynamic_to_optional(h, exp, payload);
            }
        }
        self.note_dynamic_iface(exp, span);
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

    /// A dynamic value as the union or literal type `exp` (`members`: its members, or `exp`
    /// itself): the first member whose test the value passes (`typeof` for a primitive,
    /// `instanceof` for a class, `===` for a literal, `Array.isArray` for the one array
    /// member), converted; else a `TypeError`, the member that is `any` / `unknown` itself
    /// (dynamic_whole.rs), or the one interface or object type member, converted with its own
    /// run-time check.
    fn dynamic_to_members(&mut self, h: hir::Expr, exp: TyId, members: Vec<TyId>) -> hir::Expr {
        let span = h.span;
        let dty = h.ty;
        let init = self.share_if_place(h);
        let tmp = self.new_local("<any>", dty, false, span, LocalKind::Temp);
        let name = self.cx.display(exp);
        let msg = self.str_lit(&format!("TypeError: the value is not a {name}"), span);
        let never = self.cx.ty.never;
        let mut result = self.intrinsic(Intrinsic::Panic, vec![msg], never, span);
        let catch_all = self.dynamic_member(&members, dty);
        if let Some(c) = catch_all {
            result = self.whole_as_member(tmp, dty, c, exp, span);
        }
        // One member with no test of its own (an interface or object type: `VNode` in
        // `VNode | string | number`): a value that passes no other member's test converts to
        // it, checked at run time as `x as VNode` is.
        let rest = match catch_all {
            None => self.untested_member(&members),
            Some(_) => None,
        };
        if let Some(r) = rest {
            let x = self.mk(H::Local(tmp, UseMode::Borrow), dty, span);
            let share = self.intrinsic(Intrinsic::Share, vec![x], dty, span);
            self.note_dynamic_iface(r, span);
            let value = self.intrinsic(Intrinsic::AnyTo, vec![share], r, span);
            result = self.coerce(value, exp);
        }
        let skip = |m: &TyId| Some(*m) != catch_all && Some(*m) != rest;
        let arrays = self.array_members(&members);
        for &m in members.iter().rev().filter(|m| skip(m)) {
            // `Array.isArray` tells one array member from the others, not two arrays apart.
            let tested = match self.cx.ty.kind(m) {
                TyKind::Array(_) if arrays > 1 => None,
                _ => self.member_test(tmp, dty, m, span),
            };
            let Some((test, value)) = tested else {
                if catch_all.is_some() {
                    continue;
                }
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

    /// The one member of `members` that [`Self::member_test`] has no test for, when every other
    /// member has one and it is an interface or object type (which `AnyTo` checks).
    fn untested_member(&self, members: &[TyId]) -> Option<TyId> {
        let arrays = self.array_members(members);
        let mut untested = members.iter().copied().filter(|&m| {
            let tested = match self.cx.ty.kind(m) {
                TyKind::Int(_) | TyKind::Float(_) | TyKind::Str | TyKind::Bool | TyKind::Symbol => {
                    true
                }
                TyKind::Array(_) => arrays == 1,
                _ => self.cx.lit_value(m).is_some() || self.cx.class_of(m).is_some(),
            };
            !tested
        });
        let m = untested.next()?;
        if untested.next().is_some() {
            return None;
        }
        let checked = matches!(self.cx.ty.kind(m), TyKind::Dyn(..)) || self.cx.is_object_type(m);
        (checked && self.cx.dynamic(m).is_none()).then_some(m)
    }

    /// How many of `members` are arrays.
    fn array_members(&self, members: &[TyId]) -> usize {
        let is_array = |m: &&TyId| matches!(self.cx.ty.kind(**m), TyKind::Array(_));
        members.iter().filter(is_array).count()
    }

    /// The test that the dynamic value in `tmp` (type `dty`) holds a `m`, and the value as `m`.
    pub(super) fn member_test(
        &mut self,
        tmp: LocalId,
        dty: TyId,
        m: TyId,
        span: Span,
    ) -> Option<(hir::Expr, hir::Expr)> {
        let x = self.mk(H::Local(tmp, UseMode::Borrow), dty, span);
        let owned = self.intrinsic(Intrinsic::Share, vec![x.clone()], dty, span);
        self.member_test_on(x, owned, m, span)
    }

    /// The test that the dynamic value `x` holds a `m`, and `owned` (the same value, owned) as
    /// `m`.
    pub(super) fn member_test_on(
        &mut self,
        x: hir::Expr,
        owned: hir::Expr,
        m: TyId,
        span: Span,
    ) -> Option<(hir::Expr, hir::Expr)> {
        let b = self.cx.ty.bool_;
        let dty = x.ty;
        let read = |_: &mut Self| x.clone();
        if let Some(v) = self.cx.lit_value(m) {
            let base = self.base_lit_expr(&v, span);
            let any = self.any_ty()?;
            let boxed = self.coerce(base, any);
            let x = read(self);
            let test = self.intrinsic(Intrinsic::AnyStrictEq, vec![x, boxed], b, span);
            return Some((test, self.lit_const(m, span)));
        }
        let test = match self.cx.ty.kind(m) {
            TyKind::Int(_) | TyKind::Float(_) | TyKind::Str | TyKind::Bool | TyKind::Symbol => {
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
            // The one array member (`JSXChild[]` in `VNode | string | JSXChild[]`, see
            // `dynamic_to_members`): `Array.isArray`.
            TyKind::Array(_) => {
                let x = read(self);
                if self.uses_proxy() && !self.cx.scopes[self.module].is_std {
                    self.prelude_hir_call("__anyIsArrayProxy", vec![x], b, span)
                } else {
                    self.intrinsic(Intrinsic::AnyIsArray, vec![x], b, span)
                }
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
        let value = self.intrinsic(Intrinsic::AnyTo, vec![owned], m, span);
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

    /// The variable `e` names when its type is a type parameter or an intersection with one
    /// (not a union or `| null` of it).
    pub(crate) fn own_generic_local(&mut self, e: &velt_syntax::ast::Expr) -> Option<LocalId> {
        use velt_syntax::ast::ExprKind as A;
        let name = match &e.kind {
            A::Paren(x) => return self.own_generic_local(x),
            A::Ident(i) => i.name.as_str(),
            _ => return None,
        };
        let l = self
            .f
            .scopes
            .iter()
            .rev()
            .find_map(|s| s.names.get(name).copied())?;
        let t = self.local_ty(l);
        let plain = self.cx.ty.opt_payload(t).is_none() && self.cx.union_members(t).is_none();
        (plain && self.per_instance(t)).then_some(l)
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
}
