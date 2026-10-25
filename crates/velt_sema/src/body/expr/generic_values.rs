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
use crate::generic_fns::FnParts;
use crate::hir::{self, ExprKind as H, ImplDef, TyId, TyKind};

impl FnCx<'_, '_> {
    /// Is arrow `e` a generic closure where `exp` is expected (module docs)?
    pub(super) fn is_generic_closure(&self, e: &ast::Expr, exp: Option<TyId>) -> bool {
        let ast::ExprKind::Arrow { type_params, .. } = &e.kind else {
            return false;
        };
        !type_params.is_empty() || exp.is_some_and(|t| self.cx.generic_sig(t).is_some())
    }

    /// Generic closure `e` (an arrow) as a value of generic function type `exp`, or of the
    /// generic function type of its own signature (its type parameters and typed parameters,
    /// its result inferred unless written) where none is expected.
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
        let g = exp.and_then(|t| self.cx.generic_sig(t));
        if g.is_none() && (type_params.is_empty() || params.iter().any(|p| p.ty.is_none())) {
            return self.generic_arrow_without_type(e.span);
        }
        let own = g
            .as_ref()
            .map_or(type_params.len(), |g| g.method.generics.len());
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
        let names: Vec<String> = match (&g, type_params.is_empty()) {
            (Some(g), true) => g.method.generics.names.clone(),
            _ => type_params.iter().map(|p| p.name.name.clone()).collect(),
        };
        // The type's signature in the closure's numbering: its arguments as they are here, its
        // own type parameters after the enclosing function's.
        let mut map = g.as_ref().map_or(vec![], |g| g.args.clone());
        map.push(self.cx.ty.error);
        for k in 0..own {
            map.push(self.cx.ty.param((m + k) as u32));
        }
        let saved = (
            self.env.params.clone(),
            self.bounds.clone(),
            self.constraints.clone(),
        );
        self.env.params.extend(names.iter().cloned());
        self.bounds.resize(m, vec![]);
        if !self.constraints.is_empty() {
            self.constraints.resize(m, None);
        }
        let (bounds, constraints) = match (&g, type_params.is_empty()) {
            (Some(g), true) => {
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
            _ => {
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
        let rest = (params.last()).is_some_and(|p| p.rest);
        // An error type stands for "nothing expected" (`expected_fn`): the closure's own
        // result and error type are inferred.
        let unknown = self.cx.ty.error;
        let want = match &g {
            Some(g) => {
                let pts: Vec<TyId> = (g.method.params.iter())
                    .map(|p| self.cx.subst(p.ty, &map))
                    .collect();
                let r = self.cx.subst(g.method.ret, &map);
                // A site's set is checked once the closure's own errors are known (below).
                let err = match g.method.throws {
                    _ if !self.cx.generic_fn_sites(g.iface).is_empty() => unknown,
                    Some(d) => d.ty.map_or(self.cx.ty.never, |t| self.cx.subst(t, &map)),
                    None => unknown,
                };
                self.cx.ty.intern(TyKind::FnPtr {
                    params: pts,
                    ret: r,
                    throws: err,
                    rest,
                })
            }
            None => {
                let pts: Vec<TyId> = (params.iter())
                    .map(|p| match &p.ty {
                        Some(t) => self.resolve(t),
                        None => unknown,
                    })
                    .collect();
                self.cx.ty.intern(TyKind::FnPtr {
                    params: pts,
                    ret: unknown,
                    throws: unknown,
                    rest,
                })
            }
        };
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
        let own_bounds = self.bounds[m..].to_vec();
        let own_constraints = self.constraints.get(m..).unwrap_or_default().to_vec();
        (self.env.params, self.bounds, self.constraints) = saved;
        let H::Closure(def) = h.kind else {
            return h;
        };
        let (iface, args) = match g {
            Some(g) => {
                self.closure_errors_into(&g, h.ty, &map, e.span);
                (g.iface, g.args)
            }
            None => {
                let parts = self.closure_parts(h.ty, m, own, own_bounds, own_constraints);
                let t = match parts {
                    Some(p) => self.cx.generic_fn_type(p, m, names, self.module, e.span),
                    None => self.cx.ty.error,
                };
                match self.cx.ty.kind(t).clone() {
                    TyKind::Dyn(i, a) => (i, a),
                    _ => return self.error_expr(e.span),
                }
            }
        };
        self.cx.fn_info_mut(def).escaping = true;
        let closure_ty = self.cx.ty.intern(TyKind::Closure(def));
        self.cx.impls.push(ImplDef {
            ty: closure_ty,
            generics: 0,
            iface,
            iface_args: args.clone(),
            methods: vec![def],
        });
        let impl_index = (self.cx.impls.len() - 1) as u32;
        // The closure joins the slot's dispatch group (its error type is the ABI).
        self.cx.groups = None;
        let span = h.span;
        let value = self.mk(H::Closure(def), closure_ty, span);
        let to = self.cx.ty.intern(TyKind::Dyn(iface, args));
        self.mk(
            H::ToDyn {
                expr: Box::new(value),
                impl_index,
            },
            to,
            span,
        )
    }

    /// A generic closure of function type `fty` stored into generic function type `g`
    /// written without `throws` (a site): what it throws beyond the site's set is added to the
    /// set (the program is checked again), or reported when it can't be.
    fn closure_errors_into(
        &mut self,
        g: &crate::generic_fns::GenericSig,
        fty: TyId,
        map: &[TyId],
        span: Span,
    ) {
        let sites = self.cx.generic_fn_sites(g.iface);
        if sites.is_empty() {
            return;
        }
        let TyKind::FnPtr { throws, .. } = self.cx.ty.kind(fty).clone() else {
            return;
        };
        let allowed = g.method.throws.and_then(|d| d.ty);
        let allowed = allowed.map(|t| self.cx.subst(t, map));
        let have = match allowed {
            Some(a) => self.cx.error_members(a),
            None => vec![],
        };
        let extra: Vec<TyId> = (self.cx.error_members(throws).into_iter())
            .filter(|m| *m != self.cx.ty.unit && *m != self.cx.ty.never && !have.contains(m))
            .collect();
        let Some(extra) = self.cx.error_union(&extra) else {
            return;
        };
        if !self.cx.seed_generic_sites(&sites, extra) {
            let gt = self.cx.ty.intern(TyKind::Dyn(g.iface, g.args.clone()));
            let (e, t) = (self.cx.display(extra), self.cx.display(gt));
            self.cx.error(
                Diagnostic::error(
                    format!("this function throws `{e}`, which `{t}` does not allow"),
                    span,
                )
                .with_note("write the errors it may throw in the type (`<T>(x: T) => R throws E`)"),
            );
        }
    }

    /// The parts of the generic function type of a closure of function type `fty` (its own
    /// `own` type parameters numbered after the `m` in scope), renumbered as
    /// `Ctx::generic_fn_type` takes them (the implementor before the own ones).
    fn closure_parts(
        &mut self,
        fty: TyId,
        m: usize,
        own: usize,
        bounds: Vec<Vec<Bound>>,
        constraints: Vec<Option<TyId>>,
    ) -> Option<FnParts> {
        let TyKind::FnPtr {
            params,
            ret,
            throws,
            rest,
        } = self.cx.ty.kind(fty).clone()
        else {
            return None;
        };
        let shift: Vec<TyId> = (0..m)
            .chain(m + 1..m + 1 + own)
            .map(|p| self.cx.ty.param(p as u32))
            .collect();
        let sub = |cx: &mut crate::ctx::Ctx, t: TyId| cx.subst(t, &shift);
        let throws = (throws != self.cx.ty.never).then(|| sub(self.cx, throws));
        Some(FnParts {
            site: None,
            params: params.iter().map(|t| sub(self.cx, *t)).collect(),
            ret: sub(self.cx, ret),
            throws,
            rest,
            bounds: bounds
                .into_iter()
                .map(|bs| {
                    bs.into_iter()
                        .map(|b| Bound {
                            iface: b.iface,
                            args: b.args.iter().map(|a| sub(self.cx, *a)).collect(),
                        })
                        .collect()
                })
                .collect(),
            constraints: constraints
                .into_iter()
                .map(|c| c.map(|t| sub(self.cx, t)))
                .collect(),
        })
    }

    /// Named generic function `id` where generic function type `exp` is expected: the generic
    /// closure `(a0, …) => id(a0, …)`, each call of which instantiates `id`.
    pub(super) fn generic_fn_value(&mut self, id: &ast::Ident, exp: TyId) -> hir::Expr {
        let Some(g) = self.cx.generic_sig(exp) else {
            return self.error_expr(id.span);
        };
        let n = g.method.params.len();
        let at = id.span;
        let mk = |kind| ast::Expr {
            id: ast::NodeId(u32::MAX),
            kind,
            span: at,
        };
        let name = |k: usize| ast::Ident {
            name: format!("#a{k}"),
            span: at,
        };
        let params = (0..n)
            .map(|k| ast::ArrowParam {
                name: name(k),
                pattern: None,
                ty: None,
                default: None,
                optional: false,
                rest: g.rest && k + 1 == n,
            })
            .collect();
        let args = (0..n)
            .map(|k| match g.rest && k + 1 == n {
                true => mk(ast::ExprKind::Spread(Box::new(mk(ast::ExprKind::Ident(
                    name(k),
                ))))),
                false => mk(ast::ExprKind::Ident(name(k))),
            })
            .collect();
        let call = mk(ast::ExprKind::Call {
            callee: Box::new(mk(ast::ExprKind::Ident(id.clone()))),
            type_args: vec![],
            args,
            optional: false,
        });
        let arrow = mk(ast::ExprKind::Arrow {
            type_params: vec![],
            params,
            ret: None,
            throws: None,
            body: ast::ArrowBody::Expr(Box::new(call)),
            is_async: false,
        });
        self.generic_closure(&arrow, Some(exp))
    }

    /// Generic arrow `e` where function type `exp` (not generic) is expected, as TypeScript
    /// instantiates it: its type parameters take the types its written parameter types match
    /// in `exp`'s, and it is checked as a closure with those. `None` when `e` is no such arrow
    /// or they can't all be found.
    pub(super) fn instantiated_arrow(
        &mut self,
        e: &ast::Expr,
        exp: Option<TyId>,
        escaping: bool,
    ) -> Option<hir::Expr> {
        let ast::ExprKind::Arrow {
            type_params,
            params,
            ret,
            throws,
            body,
            is_async,
        } = &e.kind
        else {
            return None;
        };
        let want = self.hint(exp)?;
        let TyKind::FnPtr { params: wanted, .. } = self.cx.ty.kind(want).clone() else {
            return None;
        };
        if type_params.is_empty() || params.iter().any(|p| p.ty.is_none()) {
            return None;
        }
        let m = self.env.params.len();
        let names: Vec<String> = type_params.iter().map(|p| p.name.name.clone()).collect();
        let saved = self.env.params.len();
        self.env.params.extend(names.iter().cloned());
        let written: Vec<TyId> = (params.iter())
            .filter_map(|p| p.ty.as_ref())
            .map(|t| self.resolve(t))
            .collect();
        self.env.params.truncate(saved);
        let mut slots = vec![None; m + names.len()];
        for (p, w) in written.iter().zip(&wanted) {
            self.cx.match_ty(*p, *w, &mut slots);
        }
        let binds: Vec<TyId> = slots[m..].iter().copied().collect::<Option<_>>()?;
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
        let n = self.env.locals.len();
        self.env.locals.extend(names.into_iter().zip(binds));
        let h = self.closure(&plain, exp, escaping);
        self.env.locals.truncate(n);
        Some(h)
    }

    /// Reports a generic arrow where no generic function type is expected (`span`).
    fn generic_arrow_without_type(&mut self, span: Span) -> hir::Expr {
        self.cx.error(
            Diagnostic::error(
                "a generic arrow function needs typed parameters, or a generic function type where it is used",
                span,
            )
            .with_note("type its parameters (`<T>(x: T) => …`), or use it where a value of a type like `<T>(x: T) => T` is expected"),
        );
        self.error_expr(span)
    }
}
