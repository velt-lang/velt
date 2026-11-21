//! `weak T` in a written type (docs/internals/design/cycles.md "`weak T`"): a weak reference to an
//! object, written in the type of a field (of a class, an object type or an interface) or a variable,
//! as `weak Node | null`.
//!
//! The places it may go resolve their written type through [`Ctx::resolve_weak_annotation`]:
//! `weak` at the top of the type, beside `null` (and `undefined`), refers to a class instance,
//! an object type, an interface value, an array or a map, and the type is the read type,
//! `T | null`. A constructor's parameter property is a field: its parameter takes the type
//! without `weak` (`without_weak`). Anywhere else
//! (parameters, results, elements, type arguments, aliases, inside another type) it is an error
//! naming those places (`Ctx::weak_misplaced`). Its lowering is not implemented yet (#3119),
//! so a well-formed `weak` is reported as such after these checks.

use velt_common::{Applicability, Diagnostic, Fix, Span};
use velt_syntax::ast::{self, TypeExprKind as K};

use crate::ctx::Ctx;
use crate::hir::{AdtKind, Nothing, TyId, TyKind};
use crate::resolve::TyEnv;

/// Where a written type has `weak` at its top, and what it refers to.
pub(crate) enum WeakShape<'a> {
    /// No `weak` at the top (one nested deeper is `weak_misplaced`'s).
    None,
    /// `weak T | null`, with `| undefined` too when `undefined`.
    Weak {
        target: &'a ast::TypeExpr,
        undefined: bool,
    },
    /// `weak T` without `null` (with `| undefined` when `undefined`): `at` is where `| null` goes.
    NoNull {
        target: &'a ast::TypeExpr,
        undefined: bool,
        at: Span,
    },
    /// `weak` beside another type than `null` and `undefined`, or twice.
    Mixed(Span),
}

/// The shape of `weak` at the top of the written type `t`.
pub(crate) fn weak_shape(t: &ast::TypeExpr) -> WeakShape<'_> {
    match &t.kind {
        K::Weak(target) => WeakShape::NoNull {
            target,
            undefined: false,
            at: Span {
                lo: t.span.hi,
                ..t.span
            },
        },
        K::Union(ms) if ms.iter().any(|m| matches!(m.kind, K::Weak(_))) => {
            let weak: Vec<&ast::TypeExpr> = ms
                .iter()
                .filter_map(|m| match &m.kind {
                    K::Weak(x) => Some(&**x),
                    _ => None,
                })
                .collect();
            let other = ms
                .iter()
                .find(|m| !matches!(m.kind, K::Weak(_) | K::Null | K::Undefined));
            match (weak.as_slice(), other) {
                ([target], None) => {
                    let null = ms.iter().any(|m| matches!(m.kind, K::Null));
                    let undefined = ms.iter().any(|m| matches!(m.kind, K::Undefined));
                    match null {
                        true => WeakShape::Weak { target, undefined },
                        false => WeakShape::NoNull {
                            target,
                            undefined,
                            at: Span {
                                lo: t.span.hi,
                                ..t.span
                            },
                        },
                    }
                }
                _ => WeakShape::Mixed(t.span),
            }
        }
        _ => WeakShape::None,
    }
}

/// The written type `t` without a `weak` at its top: `weak T | null` is `T | null`. A parameter
/// property's parameter takes this type; its field, the written one.
pub(crate) fn without_weak(t: &ast::TypeExpr) -> ast::TypeExpr {
    match &t.kind {
        K::Weak(target) => (**target).clone(),
        K::Union(ms) => ast::TypeExpr {
            kind: K::Union(
                (ms.iter())
                    .map(|m| match &m.kind {
                        K::Weak(target) => (**target).clone(),
                        _ => m.clone(),
                    })
                    .collect(),
            ),
            span: t.span,
        },
        _ => t.clone(),
    }
}

/// `T | null`, or `T | null | undefined` when `undefined` was written too.
fn both_if(undefined: bool) -> Nothing {
    match undefined {
        true => Nothing::Both,
        false => Nothing::Null,
    }
}

impl Ctx<'_> {
    /// The type written for a field or a variable, `t`, read with `resolve`: a `weak T | null` at
    /// its top is checked and resolved as `T | null`.
    pub(crate) fn resolve_weak_annotation(
        &mut self,
        t: &ast::TypeExpr,
        env: &TyEnv,
        resolve: &mut dyn FnMut(&mut Self, &ast::TypeExpr, &TyEnv) -> TyId,
    ) -> TyId {
        let (target, nothing) = match weak_shape(t) {
            WeakShape::None => return resolve(self, t, env),
            WeakShape::Mixed(span) => {
                self.err(
                    "`weak` may only be united with `null` (and `undefined`): write `weak T | null`",
                    span,
                );
                return self.ty.error;
            }
            WeakShape::NoNull {
                target,
                undefined,
                at,
            } => {
                self.error(
                    Diagnostic::error(
                        "a `weak` reference reads `null` once its target is gone: its type must include `null`",
                        t.span,
                    )
                    .with_fix(Fix::replace(
                        "add `| null`",
                        at,
                        " | null",
                        Applicability::Safe,
                    )),
                );
                (target, both_if(undefined))
            }
            WeakShape::Weak { target, undefined } => (target, both_if(undefined)),
        };
        let to = resolve(self, target, env);
        if to == self.ty.error {
            return to;
        }
        if let Err(what) = self.weak_target(to) {
            self.err(
                format!("`weak` refers to an object with an identity: {what}"),
                target.span,
            );
            return self.ty.error;
        }
        self.err("`weak` references are not implemented yet (#3119)", t.span);
        self.ty.option_of(to, nothing)
    }

    /// A `weak T` where it may not go: an error naming where it may, and `T` as if written alone.
    pub(crate) fn weak_misplaced(
        &mut self,
        t: &ast::TypeExpr,
        inner: &ast::TypeExpr,
        env: &TyEnv,
    ) -> TyId {
        self.err(
            "`weak` is allowed only at the top of the type of a field or a variable (`let`, `const` or `var`: `parent: weak Node | null`)",
            t.span,
        );
        self.resolve_type(inner, env)
    }

    /// Can a `weak` reference refer to a value of type `t`, an object with an identity: an
    /// instance of a class or of an object type, an array, a map or set, or an interface value
    /// (a reference to an object and its table)? Else what `t` is instead.
    fn weak_target(&mut self, t: TyId) -> Result<(), String> {
        let shown = self.display(t);
        match self.ty.kind(t).clone() {
            TyKind::Array(_) | TyKind::Map(..) | TyKind::Dyn(..) => Ok(()),
            TyKind::Adt(d, _) => match self.adt(d).map(|a| a.kind) {
                Some(AdtKind::Struct) => Err(format!(
                    "`{shown}` is stored by value (a struct, or an interface of fields only)"
                )),
                _ => Ok(()),
            },
            TyKind::Param(_) => Err(
                "the target is a type parameter, which may stand for a value without one".into(),
            ),
            _ => Err(format!(
                "`{shown}` is not a class, an object type, an interface, an array or a map"
            )),
        }
    }
}
