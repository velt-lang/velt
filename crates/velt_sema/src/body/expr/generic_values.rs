//! Generic closures (#943): an arrow with type parameters (`<U>(y: U): U => y`), or any arrow
//! where a generic function type is expected (`(x) => x` as a `<T>(x: T) => T`, whose
//! parameters then have the type's type parameters, as TypeScript's contextual typing gives
//! them). The closure's own type parameters follow the enclosing function's (closures share
//! their numbering), so its def is generic over both, and its value is an interface value of
//! the generic function type (`crate::generic_fns`): a `ToDyn` through an implementation for
//! the closure's own type `Closure(def)` whose one method is the closure. Lowering instantiates
//! the closure's code for each entry of the interface's table (hir_encodings.md "Generic
//! function values").

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::body::FnCx;
use crate::defs::Bound;
use crate::hir::{self, ExprKind as H, ImplDef, TyId, TyKind};

impl FnCx<'_, '_> {
    /// Is arrow `e` a generic closure where `exp` is expected (module docs)?
    pub(super) fn is_generic_closure(&self, e: &ast::Expr, exp: Option<TyId>) -> bool {
        let ast::ExprKind::Arrow { type_params, .. } = &e.kind else {
            return false;
        };
        !type_params.is_empty() || exp.is_some_and(|t| self.cx.generic_sig(t).is_some())
    }

    /// Generic closure `e` (an arrow) as a value of generic function type `exp`.
    pub(super) fn generic_closure(&mut self, e: &ast::Expr, exp: Option<TyId>) -> hir::Expr {
        let ast::ExprKind::Arrow {
            type_params,
            params,
            ret,
            throws,
            body,
            is_async,
        } = &e.kind
        else {
            unreachable!("ICE: generic closure of a non-arrow expression")
        };
        let Some(g) = exp.and_then(|t| self.cx.generic_sig(t)) else {
            return self.generic_arrow_without_type(e.span);
        };
        let own = g.method.generics.len();
        if !type_params.is_empty() && type_params.len() != own {
            let shown = self.cx.display(exp.unwrap_or(self.cx.ty.error));
            self.cx.err(
                format!(
                    "this arrow function has {} type parameter(s), but `{shown}` has {own}",
                    type_params.len()
                ),
                e.span,
            );
            return self.error_expr(e.span);
        }
        let m = self.env.params.len();
        let names: Vec<String> = match type_params.is_empty() {
            true => g.method.generics.names.clone(),
            false => type_params.iter().map(|p| p.name.name.clone()).collect(),
        };
        // The type's signature in the closure's numbering: its arguments as they are here, its
        // own type parameters after the enclosing function's.
        let mut map = g.args.clone();
        map.push(self.cx.ty.error);
        for k in 0..own {
            map.push(self.cx.ty.param((m + k) as u32));
        }
        let saved = (
            self.env.params.clone(),
            self.bounds.clone(),
            self.constraints.clone(),
        );
        self.env.params.extend(names);
        self.bounds.resize(m, vec![]);
        if !self.constraints.is_empty() {
            self.constraints.resize(m, None);
        }
        let (bounds, constraints) = match type_params.is_empty() {
            true => {
                let bounds: Vec<Vec<Bound>> = (g.method.generics.bounds.iter())
                    .map(|bs| {
                        bs.iter()
                            .map(|b| Bound {
                                iface: b.iface,
                                args: b.args.iter().map(|a| self.cx.subst(*a, &map)).collect(),
                            })
                            .collect()
                    })
                    .collect();
                let constraints: Vec<Option<TyId>> = (g.method.generics.constraints.iter())
                    .map(|c| c.map(|t| self.cx.subst(t, &map)))
                    .collect();
                (bounds, constraints)
            }
            false => {
                let b = crate::collect::shapes::resolve_bounds(self.cx, type_params, &self.env);
                (b.bounds, b.constraints)
            }
        };
        self.bounds.extend(bounds);
        self.bounds.resize(m + own, vec![]);
        if !constraints.is_empty() || !self.constraints.is_empty() {
            self.constraints.resize(m, None);
            self.constraints.extend(constraints);
            self.constraints.resize(m + own, None);
        }
        let pts: Vec<TyId> = (g.method.params.iter())
            .map(|p| self.cx.subst(p.ty, &map))
            .collect();
        let r = self.cx.subst(g.method.ret, &map);
        // Without a declared error type the closure's own is inferred (an error type stands
        // for "none expected": `expected_fn`).
        let err = match g.method.throws {
            Some(d) => d.ty.map_or(self.cx.ty.never, |t| self.cx.subst(t, &map)),
            None => self.cx.ty.error,
        };
        let rest = (params.last()).is_some_and(|p| p.rest);
        let want = self.cx.ty.intern(TyKind::FnPtr {
            params: pts,
            ret: r,
            throws: err,
            rest,
        });
        // Checked as a closure of that function type; it is stored in the interface value, so
        // it escapes.
        let plain = ast::Expr {
            id: e.id,
            kind: ast::ExprKind::Arrow {
                type_params: vec![],
                params: params.clone(),
                ret: ret.clone(),
                throws: throws.clone(),
                body: body.clone(),
                is_async: *is_async,
            },
            span: e.span,
        };
        let h = self.closure(&plain, Some(want), true);
        (self.env.params, self.bounds, self.constraints) = saved;
        let H::Closure(def) = h.kind else {
            return h;
        };
        self.cx.fn_info_mut(def).escaping = true;
        let closure_ty = self.cx.ty.intern(TyKind::Closure(def));
        self.cx.impls.push(ImplDef {
            ty: closure_ty,
            generics: 0,
            iface: g.iface,
            iface_args: g.args.clone(),
            methods: vec![def],
        });
        let impl_index = (self.cx.impls.len() - 1) as u32;
        let span = h.span;
        let value = self.mk(H::Closure(def), closure_ty, span);
        let to = self.cx.ty.intern(TyKind::Dyn(g.iface, g.args));
        self.mk(
            H::ToDyn {
                expr: Box::new(value),
                impl_index,
            },
            to,
            span,
        )
    }

    /// Reports a generic arrow where no generic function type is expected (`span`).
    fn generic_arrow_without_type(&mut self, span: Span) -> hir::Expr {
        self.cx.error(
            Diagnostic::error(
                "a generic arrow function must be the value of a `const` with typed parameters, or of a generic function type",
                span,
            )
            .with_note("declare it as `const id = <T>(x: T) => x;`, or where a value of a type like `<T>(x: T) => T` is expected"),
        );
        self.error_expr(span)
    }
}
