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

use crate::body::{FnCx, Want};
use crate::ctx::Item;
use crate::defs::Bound;
use crate::defs::{BodyState, DefInfo, GlobalInfo, GlobalSrc};
use crate::generic_fns::FnParts;

/// The local a generic function value converted to a function type is held in
/// (`FnCx::generic_as_fn`): no source can name it.
const HELD: &str = "<generic value>";
use crate::hir::{self, DefId, ExprKind as H, ImplDef, TyId, TyKind};

impl FnCx<'_, '_> {
    /// Is arrow `e` a generic closure where `exp` is expected (module docs)?
    pub(super) fn is_generic_closure(&self, e: &ast::Expr, exp: Option<TyId>) -> bool {
        let ast::ExprKind::Arrow { type_params, .. } = &e.kind else {
            return false;
        };
        // Also as an optional value (`f?: Id`, `Id | undefined`).
        !type_params.is_empty()
            || self
                .hint(exp)
                .is_some_and(|t| self.cx.generic_sig(t).is_some())
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
        let exp = self.hint(exp);
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
                // Checked against the type's once the closure's own errors are known (below).
                let err = unknown;
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
        let outer = self
            .cx
            .closure_constraints
            .replace(self.constraints.clone());
        // Messages show its own type parameters by their names.
        let shown = std::mem::replace(&mut self.cx.display_params, self.env.params.clone());
        let h = self.closure(&plain, Some(want), true);
        self.cx.display_params = shown;
        self.cx.closure_constraints = outer;
        let own_bounds = self.bounds[m..].to_vec();
        let own_constraints = self.constraints.get(m..).unwrap_or_default().to_vec();
        (self.env.params, self.bounds, self.constraints) = saved;
        let H::Closure(def) = h.kind else {
            return h;
        };
        let (iface, args) = match g {
            Some(g) => {
                self.closure_errors_into(&g, h.ty, e.span);
                (g.iface, g.args)
            }
            None => {
                let parts = self.closure_parts(h.ty, m, own, own_bounds, own_constraints, e.span);
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

    /// A generic closure of function type `fty` stored into generic function type `g`: what
    /// it throws is stored into the type (`Ctx::add_generic_errors`).
    fn closure_errors_into(&mut self, g: &crate::generic_fns::GenericSig, fty: TyId, span: Span) {
        let TyKind::FnPtr { throws, .. } = self.cx.ty.kind(fty).clone() else {
            return;
        };
        let args = g.args.clone();
        self.cx.add_generic_errors(g.iface, throws, &args, span);
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
        span: Span,
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
        let user = self.cx.scopes.get(self.module).is_some_and(|s| !s.is_std);
        Some(FnParts {
            site: user.then_some(span),
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
    ///
    /// A module-level function has one value per type, as JavaScript has one function object: a
    /// constant of the module state, made when the function is first used as a value of that
    /// type (as a class's value is, `ctor_values`), so `===`, `Map` and `Set` keys see one
    /// value.
    pub(super) fn generic_fn_value(&mut self, d: DefId, id: &ast::Ident, exp: TyId) -> hir::Expr {
        if let Some(g) = self.generic_fn_global(d, exp) {
            let read = self.global_read(g, Want::Move, id.span);
            return self.coerce(read, exp);
        }
        let arrow = self.generic_fn_arrow(id, exp);
        self.generic_closure(&arrow, Some(exp))
    }

    /// The module constant holding function `d` as a value of generic function type `exp`
    /// (made once; `None` for a local function or a type over type parameters).
    fn generic_fn_global(&mut self, d: DefId, exp: TyId) -> Option<DefId> {
        if self.cx.has_type_params(exp) || self.cx.ty.has_error(exp) {
            return None;
        }
        let f = self.cx.fn_info(d);
        let (module, qual, span) = (f.module, f.name.clone(), f.name_span);
        let name = qual.rsplit([':', '.']).next().unwrap_or(&qual).to_string();
        if !matches!(self.cx.lookup_item(module, &name), Some(Item::Def(x)) if x == d) {
            return None;
        }
        let key = (d, exp);
        let values = &self.cx.objects.class_values;
        if let Some((_, g)) = values.iter().find(|(k, _)| *k == key) {
            return Some(*g);
        }
        let info = GlobalInfo {
            name: format!("{name}#value"),
            qual_name: format!("{qual}#value"),
            module,
            span: Span::DUMMY,
            ty: exp,
            init: None,
            state: BodyState::InProgress,
            src: GlobalSrc {
                ann: None,
                init: None,
                span,
                owner: None,
                is_private: false,
            },
            mutable: false,
            stored: true,
        };
        let g = self
            .cx
            .alloc_def(Span::DUMMY, DefInfo::Global(Box::new(info)));
        self.cx.objects.class_values.push((key, g));
        let id = ast::Ident { name, span };
        let init = {
            let mut fcx = crate::body::driver::detached(self.cx, module, &[]);
            fcx.fn_name = format!("{qual}#value");
            let arrow = fcx.generic_fn_arrow(&id, exp);
            fcx.generic_closure(&arrow, Some(exp))
        };
        if let DefInfo::Global(info) = &mut self.cx.info[g.0 as usize] {
            info.init = Some(init);
            info.state = BodyState::Done;
        }
        Some(g)
    }

    /// The generic function type of generic function `d`'s own signature, for a use as a value
    /// where `exp` gives no function type to instantiate it at; `None` where it does, or for a
    /// function whose type parameters include an enclosing function's.
    pub(super) fn own_generic_type(
        &mut self,
        d: DefId,
        exp: Option<TyId>,
        span: Span,
    ) -> Option<TyId> {
        if let Some(t) = self.hint(exp) {
            let fns = match self.cx.union_members(t) {
                Some(ms) => ms,
                None => vec![t],
            };
            if fns
                .iter()
                .any(|m| matches!(self.cx.ty.kind(*m), TyKind::FnPtr { .. }))
            {
                return None;
            }
        }
        let f = self.cx.fn_info(d);
        if f.owner.is_some() || f.is_generator || f.is_async {
            return None;
        }
        let (module, qual) = (f.module, f.name.clone());
        let name = qual.rsplit([':', '.']).next().unwrap_or(&qual).to_string();
        let top = matches!(self.cx.lookup_item(module, &name), Some(Item::Def(x)) if x == d);
        if !top && !self.env.params.is_empty() {
            return None;
        }
        let f = self.cx.fn_info(d);
        let n = f.generics.len();
        let (params, throws) = (f.params.clone(), f.declared_throws.and_then(|t| t.ty));
        let generics = f.generics.clone();
        let ret = crate::body::returns::ret_of(self.cx, d, span);
        // Its type parameters after the implementor (`Ctx::generic_fn_type`'s numbering).
        let shift: Vec<TyId> = (1..=n).map(|k| self.cx.ty.param(k as u32)).collect();
        let sub = |cx: &mut crate::ctx::Ctx, t: TyId| cx.subst(t, &shift);
        let parts = FnParts {
            params: params.iter().map(|p| sub(self.cx, p.ty)).collect(),
            ret: sub(self.cx, ret),
            throws: throws.map(|t| sub(self.cx, t)),
            rest: false,
            bounds: (generics.bounds.iter())
                .map(|bs| {
                    bs.iter()
                        .map(|b| Bound {
                            iface: b.iface,
                            args: b.args.iter().map(|a| sub(self.cx, *a)).collect(),
                        })
                        .collect()
                })
                .collect(),
            constraints: (generics.constraints.iter())
                .map(|c| c.map(|t| sub(self.cx, t)))
                .collect(),
            site: None,
        };
        let t = self
            .cx
            .generic_fn_type(parts, 0, generics.names, module, span);
        (t != self.cx.ty.error).then_some(t)
    }

    /// `(a0, …) => id(a0, …)` for generic function type `exp`.
    fn generic_fn_arrow(&mut self, id: &ast::Ident, exp: TyId) -> ast::Expr {
        let Some(g) = self.cx.generic_sig(exp) else {
            return ast::Expr {
                id: ast::NodeId(u32::MAX),
                kind: ast::ExprKind::Ident(id.clone()),
                span: id.span,
            };
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
        mk(ast::ExprKind::Arrow {
            type_params: vec![],
            params,
            ret: None,
            throws: None,
            body: ast::ArrowBody::Expr(Box::new(call)),
            is_async: false,
        })
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

    /// Generic interface method `m` (as the interface declares it, `n_iface` type
    /// parameters) of an interface with arguments `iargs`, as the generic function type of a
    /// value implementing it (an object literal's member written at `span`).
    pub(super) fn method_generic_type(
        &mut self,
        m: &crate::defs::IfaceMethod,
        n_iface: usize,
        iargs: &[TyId],
        span: Span,
    ) -> TyId {
        let mm = self.env.params.len();
        let own = m.generics.len();
        let mut map = iargs.to_vec();
        map.resize(n_iface, self.cx.ty.error);
        map.push(self.cx.ty.param(mm as u32));
        for k in 0..own {
            map.push(self.cx.ty.param((mm + 1 + k) as u32));
        }
        let user = self.cx.scopes.get(self.module).is_some_and(|s| !s.is_std);
        let parts = FnParts {
            params: m.params.iter().map(|p| self.cx.subst(p.ty, &map)).collect(),
            ret: self.cx.subst(m.ret, &map),
            throws: (m.throws.and_then(|t| t.ty)).map(|t| self.cx.subst(t, &map)),
            rest: false,
            bounds: (m.generics.bounds.iter())
                .map(|bs| {
                    bs.iter()
                        .map(|b| Bound {
                            iface: b.iface,
                            args: b.args.iter().map(|a| self.cx.subst(*a, &map)).collect(),
                        })
                        .collect()
                })
                .collect(),
            constraints: (m.generics.constraints.iter())
                .map(|c| c.map(|t| self.cx.subst(t, &map)))
                .collect(),
            site: user.then_some(span),
        };
        let names = m.generics.names.clone();
        self.cx.generic_fn_type(parts, mm, names, self.module, span)
    }

    /// Generic function value `h` as a value of function type `exp`, as TypeScript instantiates
    /// it: the closure `(a0, …) => g(a0, …)` over `g`, a local holding `h`, whose call
    /// instantiates `h` at `exp`'s parameter types. `Err` when it doesn't take them.
    pub(super) fn generic_as_fn(
        &mut self,
        h: hir::Expr,
        exp: TyId,
    ) -> Result<hir::Expr, hir::Expr> {
        let TyKind::FnPtr { params, rest, .. } = self.cx.ty.kind(exp).clone() else {
            return Err(h);
        };
        // A function value may take fewer parameters than its type gives.
        let takes = self
            .cx
            .generic_sig(h.ty)
            .map_or(0, |g| g.method.params.len());
        let rest = rest && takes >= params.len();
        let params = &params[..params.len().min(takes)];
        let span = h.span;
        self.push_scope();
        let (ty, at) = (h.ty, h.span);
        let l = self.new_local(HELD, ty, false, at, crate::body::LocalKind::Temp);
        let scope = self.f.scopes.last_mut().expect("ICE: no scope");
        scope.names.insert(HELD.to_string(), l);
        let mut h = h;
        crate::body::places::set_place_mode(&mut h, hir::UseMode::Move);
        let stmts = vec![hir::Stmt {
            kind: hir::StmtKind::Let {
                local: l,
                init: Some(h),
            },
            span: at,
        }];
        let n = params.len();
        let mk = |kind| ast::Expr {
            id: ast::NodeId(u32::MAX),
            kind,
            span,
        };
        let name = |k: usize| ast::Ident {
            name: format!("#a{k}"),
            span,
        };
        let ps = (0..n)
            .map(|k| ast::ArrowParam {
                name: name(k),
                pattern: None,
                ty: None,
                default: None,
                optional: false,
                rest: rest && k + 1 == n,
            })
            .collect();
        let args = (0..n)
            .map(|k| match rest && k + 1 == n {
                true => mk(ast::ExprKind::Spread(Box::new(mk(ast::ExprKind::Ident(
                    name(k),
                ))))),
                false => mk(ast::ExprKind::Ident(name(k))),
            })
            .collect();
        let held = ast::Ident {
            name: HELD.into(),
            span,
        };
        let call = mk(ast::ExprKind::Call {
            callee: Box::new(mk(ast::ExprKind::Ident(held))),
            type_args: vec![],
            args,
            optional: false,
        });
        let arrow = mk(ast::ExprKind::Arrow {
            type_params: vec![],
            params: ps,
            ret: None,
            throws: None,
            body: ast::ArrowBody::Expr(Box::new(call)),
            is_async: false,
        });
        let value = self.closure(&arrow, Some(exp), true);
        self.pop_scope();
        let value = self.coerce(value, exp);
        let block = hir::Block {
            stmts,
            value: Some(Box::new(value)),
            span,
        };
        Ok(self.mk(H::Block(block), exp, span))
    }

    /// A generic function value (of type `g`) passed for a parameter of function type `pat`
    /// (over a call's type parameters, `slots` those known so far): the value is instantiated
    /// at `pat`'s parameter types (`generic_as_fn`), so its result there fixes the slots of
    /// `pat`'s result (`xs.map(id)` with `map<U>(f: (x: T) => U)`).
    pub(super) fn infer_from_generic(&mut self, pat: TyId, g: TyId, slots: &mut [Option<TyId>]) {
        let TyKind::FnPtr { params, ret, .. } = self.cx.ty.kind(pat).clone() else {
            return;
        };
        let Some(sig) = self.cx.generic_sig(g) else {
            return;
        };
        let known: Vec<TyId> = slots
            .iter()
            .map(|s| s.unwrap_or(self.cx.ty.error))
            .collect();
        let n = sig.args.len();
        let own = sig.method.generics.len();
        // Its own type parameters as `0..own`, its arguments in place.
        let mut map = sig.args.clone();
        map.push(self.cx.ty.error);
        map.extend((0..own).map(|k| self.cx.ty.param(k as u32)));
        let mut own_slots = vec![None; own];
        for (gp, pp) in sig.method.params.iter().zip(&params) {
            let gp = self.cx.subst(gp.ty, &map);
            let pp = self.cx.subst(*pp, &known);
            if !self.cx.ty.has_error(pp) {
                self.cx.match_ty(gp, pp, &mut own_slots);
            }
        }
        let Some(own_args) = own_slots.into_iter().collect::<Option<Vec<TyId>>>() else {
            return;
        };
        let mut inst = sig.args.clone();
        inst.resize(n, self.cx.ty.error);
        inst.push(self.cx.ty.error);
        inst.extend(own_args);
        let r = self.cx.subst(sig.method.ret, &inst);
        self.cx.match_ty(ret, r, slots);
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
