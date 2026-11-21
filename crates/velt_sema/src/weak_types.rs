//! `weak T` in a written type (docs/internals/design/cycles.md "`weak T`"): a weak reference to an
//! object, written in the type of a field (of a class, an object type or an interface) or a variable,
//! as `weak Node | null`.
//!
//! The places it may go resolve their written type through [`Ctx::resolve_weak_annotation`]:
//! `weak` at the top of the type, beside `null` (and `undefined`), refers to a class instance,
//! an object type, an array or a map, and the type is the read type, `T | null`. Anywhere else
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
    /// `weak T` without `null`: `at` is where `| null` goes.
    NoNull { target: &'a ast::TypeExpr, at: Span },
    /// `weak` beside another type than `null` and `undefined`, or twice.
    Mixed(Span),
}

/// The shape of `weak` at the top of the written type `t`.
pub(crate) fn weak_shape(t: &ast::TypeExpr) -> WeakShape<'_> {
    match &t.kind {
        K::Weak(target) => WeakShape::NoNull {
            target,
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
            WeakShape::NoNull { target, at } => {
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
                (target, Nothing::Null)
            }
            WeakShape::Weak { target, undefined } => {
                let nothing = match undefined {
                    true => Nothing::Both,
                    false => Nothing::Null,
                };
                (target, nothing)
            }
        };
        let to = resolve(self, target, env);
        if to == self.ty.error {
            return to;
        }
        if !self.weak_target(to) {
            let shown = self.display(to);
            self.err(
                format!(
                    "`weak` refers to an object: `{shown}` is not a class, an object type, an array or a map"
                ),
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
            "`weak` is allowed only at the top of the type of a field or a `let` or `const` variable (`parent: weak Node | null`)",
            t.span,
        );
        self.resolve_type(inner, env)
    }

    /// Can a `weak` reference refer to a value of type `t`: an object with an identity?
    fn weak_target(&mut self, t: TyId) -> bool {
        match self.ty.kind(t).clone() {
            TyKind::Array(_) | TyKind::Map(..) => true,
            // A struct (and a field-only interface) is a value stored inline, without an identity.
            TyKind::Adt(d, _) => self.adt(d).is_some_and(|a| a.kind != AdtKind::Struct),
            _ => false,
        }
    }
}
