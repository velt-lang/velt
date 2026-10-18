//! JavaScript's `Object` built-ins beyond `keys` / `values` / `entries` (object_keys.rs), and
//! `Array.isArray` (docs/reference/types.md "`Object` built-ins"). Each is checked here against the static types
//! of its arguments and becomes ordinary code over them; a program that doesn't call them gets
//! none of it.
//!
//! - `Object.is(a, b)`: JavaScript's SameValue (`Intrinsic::SameValue`): `===`, except that `NaN`
//!   is itself and `0` is not `-0`. Values of unrelated types are never the same.
//! - `Array.isArray(x)`: a test of the array members of `x`'s type, which narrows `x` too
//!   (`body::narrow`); a `JsonValue`'s `isArray()`.
//! - `Object.assign(target, ...sources)`: each source's own fields written to the target, in
//!   order, through its setters; the target itself is the result. A record target takes any
//!   keys; an object or class target only the fields its type declares.
//! - `Object.freeze(o)` of a fresh object literal: the object, typed with every field readonly.
//!   Its type is a definition of its own (`Ctx::frozen_defs`) that doesn't convert to a type
//!   whose fields can be written, so no write reaches it (Node throws a `TypeError` on one).
//! - `Object.fromEntries(pairs)`: a `Record<string, V>` (std/prelude/record.vlt); an iterable
//!   that is not an array goes through `Array.from` first.
//! - `Object.hasOwn(o, k)` and `Object.prototype.hasOwnProperty.call(o, k)`: a record's entry
//!   test, or for other objects whether `Object.keys(o)` lists `k`.
//! - `Object.prototype.toString.call(v)`: the `[object Tag]` string of each member type.
//! - `Object.getPrototypeOf(v)`, `Object.prototype`, `C.prototype`: an `ObjectPrototype` value
//!   naming the prototype (std/prelude/record.vlt), which `===` compares by what it names
//!   (`__prototypeSame`, from `binary`).
//!
//! What Velt's fixed object layouts can't do exactly is an error saying it is not supported yet,
//! never a silent difference: other `Object` functions, prototypes of built-in values whose
//! behaviour as objects Velt doesn't model (`Date.prototype`, `String.prototype`), and adding
//! fields a type doesn't declare. The operations are each one function taking checked operands,
//! so that a `Proxy` (#823) can add its traps in one place.
//!
//! `Object.assign` is in `object_assign.rs`, own keys and `[object Tag]` strings in
//! `object_own.rs`, prototypes in `object_prototypes.rs`.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::setters::synth;
use crate::body::{FnCx, Want};
use crate::ctx::Item;
use crate::hir::{self, ExprKind as H, Intrinsic, TyId, TyKind};

/// The functions of JavaScript's `Object` that Velt doesn't have yet (`keys`, `values`,
/// `entries` and the ones here are handled).
const NOT_YET: [&str; 15] = [
    "defineProperty",
    "defineProperties",
    "getOwnPropertyNames",
    "getOwnPropertyDescriptor",
    "getOwnPropertyDescriptors",
    "getOwnPropertySymbols",
    "isFrozen",
    "isSealed",
    "isExtensible",
    "seal",
    "preventExtensions",
    "setPrototypeOf",
    "groupBy",
    "fromAsync",
    "hasOwnProperty",
];

impl FnCx<'_, '_> {
    /// A call of one of the built-ins this module handles (`None`: another call).
    pub(crate) fn object_builtin_call(
        &mut self,
        callee: &ast::Expr,
        type_args: &[ast::TypeExpr],
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> Option<hir::Expr> {
        let ast::ExprKind::Member {
            object,
            prop,
            optional: false,
        } = &callee.kind
        else {
            return None;
        };
        if prop.name == "call" {
            if let Some(m) = self.object_prototype_method(object) {
                return Some(self.prototype_method_call(&m, args, span));
            }
        }
        let ast::ExprKind::Ident(ns) = &object.kind else {
            return None;
        };
        if ns.name == "Array" && prop.name == "isArray" && self.builtin_array(ns) {
            // An `any` / `unknown` argument is `dynamic_narrow`'s, checked before.
            return Some(self.array_is_array(args, span));
        }
        if !self.is_prelude_object(ns) {
            return None;
        }
        let name = prop.name.as_str();
        let known = matches!(
            name,
            "is" | "assign" | "freeze" | "fromEntries" | "hasOwn" | "getPrototypeOf" | "create"
        );
        if !known && !NOT_YET.contains(&name) {
            return None;
        }
        if !type_args.is_empty() && name != "fromEntries" {
            self.cx
                .err(format!("`Object.{name}` takes no type arguments"), span);
        }
        if let Some(h) = self.reject_dynamic_args(&format!("Object.{name}"), args, span) {
            return Some(h);
        }
        Some(match name {
            "is" => self.object_is(args, span),
            "assign" => self.object_assign(args, Want::Borrow, span),
            "freeze" => self.object_freeze(args, span),
            "fromEntries" => self.object_from_entries(callee, type_args, args, exp, span)?,
            "hasOwn" => self.has_own(args, "Object.hasOwn", span),
            "getPrototypeOf" => self.get_prototype_of(args, span),
            "create" => self.object_create(args, exp, span),
            _ => {
                let mut d =
                    Diagnostic::error(format!("`Object.{name}` is not supported yet"), prop.span);
                if name == "defineProperty" || name == "defineProperties" {
                    d = d.with_note("Velt objects have the fields their type declares (#922)");
                }
                self.cx.error(d);
                self.check_args_loose(args);
                self.error_expr(span)
            }
        })
    }

    /// An error when an argument of `what` is `any` or `unknown` (`None`: none is), whose
    /// run-time tests are #924's.
    pub(super) fn reject_dynamic_args(
        &mut self,
        what: &str,
        args: &[ast::Expr],
        span: Span,
    ) -> Option<hir::Expr> {
        let at = args.iter().find(|a| {
            self.trial_type(a)
                .is_some_and(|t| self.cx.dynamic(t).is_some())
        })?;
        self.cx.err(
            format!("`{what}` of an `any` or `unknown` value is not supported yet (#924)"),
            at.span,
        );
        self.check_args_loose(args);
        Some(self.error_expr(span))
    }

    /// Does `ns` name the prelude's `Object` (not shadowed)?
    pub(crate) fn is_prelude_object(&mut self, ns: &ast::Ident) -> bool {
        if ns.name != "Object" || self.is_local_name("Object") {
            return false;
        }
        let prelude = self.cx.prelude_adt("Object");
        matches!(
            self.cx.lookup_item_at(self.module, "Object", ns.span),
            Some(Item::Def(d)) if Some(d) == prelude
        )
    }

    /// Does `ns` name the built-in `Array` (no local, function or class of that name)?
    pub(super) fn builtin_array(&mut self, ns: &ast::Ident) -> bool {
        ns.name == "Array"
            && !self.is_local_name("Array")
            && self
                .cx
                .lookup_item_at(self.module, "Array", ns.span)
                .is_none()
    }

    /// Is `callee` `Array.isArray` (the built-in), and `args` its one argument?
    pub(crate) fn is_array_test<'a>(
        &mut self,
        callee: &ast::Expr,
        args: &'a [ast::Expr],
    ) -> Option<&'a ast::Expr> {
        let ast::ExprKind::Member {
            object,
            prop,
            optional: false,
        } = &callee.kind
        else {
            return None;
        };
        let (ast::ExprKind::Ident(ns), [arg]) = (&object.kind, args) else {
            return None;
        };
        (prop.name == "isArray" && self.builtin_array(ns)).then_some(arg)
    }

    /// Wrong number of arguments for `what` (TypeScript's TS2554).
    pub(super) fn arity_error(
        &mut self,
        what: &str,
        want: usize,
        args: &[ast::Expr],
        span: Span,
    ) -> hir::Expr {
        self.cx.err(
            format!(
                "`{what}` expects {want} argument(s), but got {} (TypeScript's TS2554)",
                args.len()
            ),
            span,
        );
        self.check_args_loose(args);
        self.error_expr(span)
    }

    /// `Array.isArray(x)`.
    pub(super) fn array_is_array(&mut self, args: &[ast::Expr], span: Span) -> hir::Expr {
        let [arg] = args else {
            return self.arity_error("Array.isArray", 1, args, span);
        };
        let json = |s: &Self, t: TyId| matches!(s.cx.ty.kind(t), TyKind::Adt(d, _) if s.cx.is_json_value(*d));
        if let Some(t) = self.trial_type(arg) {
            let nullable = self.cx.ty.opt_payload(t);
            if json(self, nullable.unwrap_or(t)) {
                // `v.isArray()`, or `v?.isArray() ?? false` for a `JsonValue | null`.
                let call = synth(
                    ast::ExprKind::Call {
                        callee: Box::new(synth(
                            ast::ExprKind::Member {
                                object: Box::new(arg.clone()),
                                prop: ast::Ident {
                                    name: "isArray".to_string(),
                                    span,
                                },
                                optional: nullable.is_some(),
                            },
                            span,
                        )),
                        type_args: vec![],
                        args: vec![],
                        optional: false,
                    },
                    span,
                );
                let e = match nullable {
                    None => call,
                    Some(_) => synth(
                        ast::ExprKind::Binary {
                            op: ast::BinaryOp::Nullish,
                            lhs: Box::new(call),
                            rhs: Box::new(synth(ast::ExprKind::Lit(ast::Lit::Bool(false)), span)),
                        },
                        span,
                    ),
                };
                return self.expr(&e, None, Want::Move);
            }
            let members = self
                .cx
                .union_members(nullable.unwrap_or(t))
                .unwrap_or_default();
            if members.iter().any(|m| json(self, *m)) {
                let tn = self.cx.display(t);
                self.cx.err(
                    format!("`Array.isArray` of a `{tn}` is not supported yet: test the `JsonValue` member with `isArray()`"),
                    arg.span,
                );
                return self.error_expr(span);
            }
        }
        let s = self.expr(arg, None, Want::Borrow);
        if self.cx.ty.is_bottom(s.ty) {
            return self.error_expr(span);
        }
        let b = self.cx.ty.bool_;
        if self.cx.dynamic(s.ty).is_some() {
            // An `any` / `unknown` of any expression shape: whether the value held is an array.
            return self.intrinsic(hir::Intrinsic::AnyIsArray, vec![s], b, span);
        }
        let core = self.cx.ty.opt_payload(s.ty).unwrap_or(s.ty);
        let mut members = self.cx.union_members(core).unwrap_or_default();
        members.push(core);
        if let Some(&p) = members
            .iter()
            .find(|&&m| matches!(self.cx.ty.kind(m), TyKind::Param(_)))
        {
            // Which type argument it holds is known only per instantiation.
            let tn = self.cx.display(p);
            self.cx.err(
                format!("`Array.isArray` of a value of the type parameter `{tn}` is not supported yet: test a value of a concrete type, or of type `unknown`"),
                arg.span,
            );
            return self.error_expr(span);
        }
        self.type_test(s, &is_array_ty, &|_| false, span).expr
    }

    /// `Object.is(a, b)`.
    pub(super) fn object_is(&mut self, args: &[ast::Expr], span: Span) -> hir::Expr {
        let [a, b] = args else {
            return self.arity_error("Object.is", 2, args, span);
        };
        // Against `null` or `undefined`, SameValue is `===`.
        let nothing = |e: &ast::Expr| match &strip_parens(e).kind {
            ast::ExprKind::Lit(l @ (ast::Lit::Null | ast::Lit::Undefined)) => Some(l.clone()),
            _ => None,
        };
        if let (Some(x), Some(y)) = (nothing(a), nothing(b)) {
            // `Object.is(null, null)`: `null` is itself, and not `undefined`.
            let same = std::mem::discriminant(&x) == std::mem::discriminant(&y);
            let b = self.cx.ty.bool_;
            return self.mk(H::Lit(hir::Lit::Bool(same)), b, span);
        }
        if nothing(a).is_some() || nothing(b).is_some() {
            let eq = synth(
                ast::ExprKind::Binary {
                    op: ast::BinaryOp::StrictEq,
                    lhs: Box::new(a.clone()),
                    rhs: Box::new(b.clone()),
                },
                span,
            );
            return self.expr(&eq, None, Want::Move);
        }
        let (l, r) = self.operands(a, b, None, Want::Borrow);
        if self.cx.ty.is_bottom(l.ty) || self.cx.ty.is_bottom(r.ty) {
            return self.error_expr(span);
        }
        let (l, r) = (self.unbrand(l), self.unbrand(r));
        let (l, r) = self.nullable_operands(l, r);
        let (l, r) = self.union_operands(l, r);
        let (l, r) = self.identity_operands(l, r);
        let (l, r) = if l.ty != r.ty || self.cx.lit_value(l.ty).is_some() {
            (self.widen_value(l), self.widen_value(r))
        } else {
            (l, r)
        };
        let (l, r) = self.mix_numbers(l, r);
        let b = self.cx.ty.bool_;
        if l.ty != r.ty {
            // Values of unrelated types are never the same value: both are evaluated, in order.
            let stmts = [l, r]
                .into_iter()
                .map(|e| hir::Stmt {
                    span: e.span,
                    kind: hir::StmtKind::Expr(e),
                })
                .collect();
            let no = self.mk(H::Lit(hir::Lit::Bool(false)), b, span);
            let block = hir::Block {
                stmts,
                value: Some(Box::new(no)),
                span,
            };
            return self.mk(H::Block(block), b, span);
        }
        self.intrinsic(Intrinsic::SameValue, vec![l, r], b, span)
    }

    /// `Object.freeze(o)`.
    pub(super) fn object_freeze(&mut self, args: &[ast::Expr], span: Span) -> hir::Expr {
        let [arg] = args else {
            return self.arity_error("Object.freeze", 1, args, span);
        };
        let fresh = matches!(strip_parens(arg).kind, ast::ExprKind::Object(_));
        let h = self.expr(arg, None, Want::Move);
        if self.cx.ty.is_bottom(h.ty) {
            return self.error_expr(span);
        }
        let frozen = fresh
            .then(|| self.cx.frozen_type(h.ty, self.module))
            .flatten();
        let Some(frozen) = frozen else {
            let tn = self.cx.display(h.ty);
            self.cx.error(
                Diagnostic::error(
                    format!("`Object.freeze` of a `{tn}` is not supported yet"),
                    arg.span,
                )
                .with_note("Velt freezes a new object literal, `Object.freeze({ ... })`, whose fields are then readonly; for other values, use a `Readonly<T>` type"),
            );
            return self.error_expr(span);
        };
        self.coerce(h, frozen)
    }

    /// `Object.fromEntries(pairs)`: the prelude's, with an iterable that is not an array
    /// passed through `Array.from` (`None`: the prelude call as written).
    pub(super) fn object_from_entries(
        &mut self,
        callee: &ast::Expr,
        type_args: &[ast::TypeExpr],
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> Option<hir::Expr> {
        let [arg] = args else {
            return None;
        };
        if matches!(strip_parens(arg).kind, ast::ExprKind::Array(_)) {
            return None;
        }
        let ty = self.trial_type(arg);
        if ty.is_none_or(|t| {
            matches!(self.cx.ty.kind(t), TyKind::Array(_)) || self.cx.ty.is_bottom(t)
        }) {
            return None;
        }
        // `Array.from(pairs)`: a `Map`'s entries, a generator's values.
        let from = synth(
            ast::ExprKind::Call {
                callee: Box::new(synth(
                    ast::ExprKind::Member {
                        object: Box::new(synth(
                            ast::ExprKind::Ident(ast::Ident {
                                name: "Array".to_string(),
                                span: arg.span,
                            }),
                            arg.span,
                        )),
                        prop: ast::Ident {
                            name: "from".to_string(),
                            span: arg.span,
                        },
                        optional: false,
                    },
                    arg.span,
                )),
                type_args: vec![],
                args: vec![arg.clone()],
                optional: false,
            },
            arg.span,
        );
        let ast::ExprKind::Member { object, prop, .. } = &callee.kind else {
            return None;
        };
        Some(self.member_call(object, prop, false, type_args, &[from], exp, span))
    }

    /// The type `e` checks to, without keeping anything of the check (`None`: it has errors).
    pub(super) fn trial_type(&mut self, e: &ast::Expr) -> Option<TyId> {
        let mark = crate::body::recheck::Mark::here(self.cx);
        let frames = self.trial_frames();
        let diags = self.cx.diags.len();
        let temps = self.call_temps.len();
        let ty = self.expr(e, None, Want::Borrow).ty;
        let failed = self.cx.diags.len() > diags;
        mark.rollback(self.cx);
        self.cx.diags.truncate(diags);
        self.call_temps.truncate(temps);
        self.restore_trial_frames(frames);
        (!failed).then_some(ty)
    }

    /// `Object.create(proto)`: `Object.create(null)` where a `Record<string, V>` is expected.
    pub(super) fn object_create(
        &mut self,
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        let [proto] = args else {
            if args.len() == 2 {
                self.cx.err(
                    "`Object.create` with property descriptors is not supported yet",
                    args[1].span,
                );
                self.check_args_loose(args);
                return self.error_expr(span);
            }
            return self.arity_error("Object.create", 1, args, span);
        };
        if !matches!(strip_parens(proto).kind, ast::ExprKind::Lit(ast::Lit::Null)) {
            self.cx.error(
                Diagnostic::error(
                    "`Object.create` with a prototype other than `null` is not supported yet",
                    proto.span,
                )
                .with_note("Velt objects get their prototype from their type: a class instance from its class, any other object from `Object.prototype`"),
            );
            self.check_args_loose(args);
            return self.error_expr(span);
        }
        let want = self.hint(exp);
        let record = want.filter(|t| {
            self.record_args(*t)
                .is_some_and(|(k, _)| k == self.cx.ty.str_)
        });
        let Some(record) = record else {
            let mut d = Diagnostic::error(
                "`Object.create(null)` makes a `Record<string, V>`: it needs a variable or parameter of that type",
                span,
            );
            if let Some(t) = want {
                let tn = self.cx.display(t);
                d = Diagnostic::error(
                    format!("`Object.create(null)` as a `{tn}` is not supported yet: it makes a `Record<string, V>`"),
                    span,
                );
            }
            self.cx.error(
                d.with_note("write `const d: Record<string, number> = Object.create(null);`"),
            );
            return self.error_expr(span);
        };
        let empty = synth(ast::ExprKind::Object(vec![]), span);
        let h = self.expr(&empty, Some(record), Want::Move);
        let mut lets = vec![];
        let r = self.temp(&format!("<create@{}>", span.lo), h, &mut lets);
        let mark = self.record_call(r.clone(), "__markNullProto", &[], span);
        lets.push(hir::Stmt {
            span,
            kind: hir::StmtKind::Expr(mark),
        });
        self.with_lets(lets, r)
    }
}

/// `e` without parentheses.
pub(super) fn strip_parens(e: &ast::Expr) -> &ast::Expr {
    match &e.kind {
        ast::ExprKind::Paren(inner) => strip_parens(inner),
        _ => e,
    }
}

/// Is `t` an array type (a tuple is an array in JavaScript)?
pub(crate) fn is_array_ty(cx: &crate::ctx::Ctx, t: TyId) -> bool {
    matches!(cx.ty.kind(t), TyKind::Array(_) | TyKind::Tuple(_))
}
