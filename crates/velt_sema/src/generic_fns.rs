//! Generic function values (#943, option A: a table of instantiations). TypeScript's
//! `<T>(x: T) => T` is the type of a value that stays generic: each call instantiates it. Velt
//! gives such a type an interface of one generic method, `#call<T>(x: T): T`, made here once per
//! shape (`<U>(y: U) => U` is the same type) over the type parameters in scope that the shape
//! mentions (its arguments: `EmitFn<E>`'s `E`). Its values are interface values, so a call
//! `f(1)` is a call of `#call` through the value's table (`Callee::Dyn` with the method's type
//! arguments), and lowering gives the slot one entry per instantiation the program reaches
//! (hir_encodings.md "Generic function values"). What implements it is a generic closure (an
//! arrow with type parameters, or one checked against the type), whose code lowering
//! instantiates per entry; a value of the type converts to itself.
//!
//! A generic function type is written with type parameters (`<T>(x: T) => R`, also as an
//! interface or object type with one generic call signature, `crate::call_sigs`, and as a
//! generic method of an object type, `{ run<T>(fn: () => T): T }`). A constant of such a type
//! with an arrow or name initializer is a generic function instead (`generic_arrows::typed`).

use rustc_hash::FxHashMap;

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::ctx::Ctx;
use crate::defs::{Bound, DefInfo, Generics, IfaceInfo, IfaceMethod, ParamSig};
use crate::hir::{DefId, TyId, TyKind};
use crate::resolve::TyEnv;

/// The name of the one method of a generic function type's interface. Not a name source can
/// spell, so member access never finds it.
pub(crate) const CALL: &str = "#call";

/// What makes two generic function types one: the signature over the interface's numbering
/// (its arguments `0..n`, the implementor `n`, the own type parameters after it), the own
/// parameters' bounds, and `n`.
#[derive(Clone, PartialEq, Eq, Hash)]
struct Shape {
    params: Vec<TyId>,
    ret: TyId,
    throws: Option<TyId>,
    rest: bool,
    bounds: Vec<Vec<(DefId, Vec<TyId>)>>,
    constraints: Vec<Option<TyId>>,
    outer: usize,
}

/// The interfaces made for generic function types.
#[derive(Default)]
pub(crate) struct GenericFns {
    /// Per shape, with the error-set variables of its types left out.
    by_plain: FxHashMap<Shape, DefId>,
    /// Per interface made here: whether its last parameter is a rest parameter.
    made: FxHashMap<DefId, bool>,
    /// The type parameter names each was first written with, for showing it.
    names: FxHashMap<DefId, Vec<String>>,
    /// The site of each one's error type: its first spelling, when in a user module.
    sites: FxHashMap<DefId, Span>,
}

/// A generic function type's parts, over the type parameters in scope, the implementor and
/// its own type parameters (`Ctx::generic_fn_type`).
pub(crate) struct FnParts {
    pub params: Vec<TyId>,
    pub ret: TyId,
    pub throws: Option<TyId>,
    pub rest: bool,
    pub bounds: Vec<Vec<Bound>>,
    pub constraints: Vec<Option<TyId>>,
    /// Where it is written, in a user module (`Ctx::generic_fn_type`).
    pub site: Option<Span>,
}

/// A generic function type's signature in terms of the type parameters in scope where it is
/// used (`TyKind::Dyn`'s arguments substituted), its own type parameters after them.
pub(crate) struct GenericSig {
    pub iface: DefId,
    pub args: Vec<TyId>,
    /// The method: params, result, own generics, declared error type.
    pub method: IfaceMethod,
    /// The last parameter is a rest parameter.
    pub rest: bool,
}

impl Ctx<'_> {
    /// `<T…>(params) => ret` written in `env`: the interface value type of its shape.
    pub(crate) fn resolve_generic_fn(
        &mut self,
        type_params: &[ast::GenericParam],
        inner: &ast::TypeExpr,
        env: &TyEnv,
        span: Span,
    ) -> TyId {
        let ast::TypeExprKind::Function {
            params,
            ret,
            throws,
            rest,
        } = &inner.kind
        else {
            let (msg, note) = crate::fn_types::unsupported_fn_type(&inner.kind);
            self.error(Diagnostic::error(msg, span).with_note(note));
            return self.ty.error;
        };
        let m = env.params.len();
        let mut names = env.params.clone();
        // Not a writable name: the implementor.
        names.push("<Self>".into());
        names.extend(type_params.iter().map(|g| g.name.name.clone()));
        let inner_env = TyEnv {
            params: names,
            ..env.clone()
        };
        let pts: Vec<TyId> = params
            .iter()
            .map(|t| self.resolve_type(t, &inner_env))
            .collect();
        let ret = self.resolve_type(ret, &inner_env);
        let written = throws.as_ref().map(|t| {
            let e = self.resolve_type(t, &inner_env);
            self.no_void_error(e, t.span);
            e
        });
        let throws = written;
        // Where it is written in a user module: the site of its interface's errors, if it is
        // the first spelling (`Ctx::generic_fn_type`).
        let user = self.scopes.get(env.module).is_some_and(|s| !s.is_std);
        let site = (user && span != Span::DUMMY).then_some(span);
        let b = crate::collect::shapes::resolve_bounds(self, type_params, &inner_env);
        if [ret]
            .iter()
            .chain(&pts)
            .chain(&throws)
            .any(|t| self.ty.has_error(*t))
        {
            return self.ty.error;
        }
        let parts = FnParts {
            params: pts,
            ret,
            throws,
            rest: *rest,
            bounds: b.bounds,
            constraints: b.constraints,
            site,
        };
        let own_names: Vec<String> = type_params.iter().map(|g| g.name.name.clone()).collect();
        self.generic_fn_type(parts, m, own_names, env.module, span)
    }

    /// The generic function type of `parts`, written over the `m` type parameters in scope,
    /// the implementor (`Param(m)`) and its own type parameters named `own_names` (`Param(m +
    /// 1 + k)`).
    pub(crate) fn generic_fn_type(
        &mut self,
        parts: FnParts,
        m: usize,
        own_names: Vec<String>,
        module: usize,
        span: Span,
    ) -> TyId {
        // The type parameters in scope it mentions become the interface's.
        let mut mentioned = vec![];
        let mut walk = |cx: &Ctx, t: TyId| crate::types::collect_params(&cx.ty, t, &mut mentioned);
        for t in parts.params.iter().chain([&parts.ret]).chain(&parts.throws) {
            walk(self, *t);
        }
        for bs in &parts.bounds {
            for bound in bs {
                for a in &bound.args {
                    walk(self, *a);
                }
            }
        }
        for t in parts.constraints.iter().flatten() {
            walk(self, *t);
        }
        let mut outer: Vec<u32> = mentioned
            .into_iter()
            .filter(|p| (*p as usize) < m)
            .collect();
        outer.sort_unstable();
        outer.dedup();
        let n = outer.len();
        let own = own_names.len();
        let mut map: Vec<TyId> = vec![self.ty.error; m + 1 + own];
        for (i, p) in outer.iter().enumerate() {
            map[*p as usize] = self.ty.param(i as u32);
        }
        map[m] = self.ty.param(n as u32);
        for k in 0..own {
            map[m + 1 + k] = self.ty.param((n + 1 + k) as u32);
        }
        let renumber = |cx: &mut Ctx, t: TyId| cx.subst(t, &map);
        let mut bounds: Vec<Vec<(DefId, Vec<TyId>)>> = parts
            .bounds
            .iter()
            .map(|bs| {
                bs.iter()
                    .map(|x| {
                        let args = x.args.iter().map(|a| renumber(self, *a)).collect();
                        (x.iface, args)
                    })
                    .collect()
            })
            .collect();
        bounds.resize(own, vec![]);
        let mut constraints: Vec<Option<TyId>> = parts
            .constraints
            .iter()
            .map(|c| c.map(|t| renumber(self, t)))
            .collect();
        if constraints.iter().all(Option::is_none) {
            constraints.clear();
        } else {
            constraints.resize(own, None);
        }
        let shape = Shape {
            params: parts.params.iter().map(|t| renumber(self, *t)).collect(),
            ret: renumber(self, parts.ret),
            // Not part of the type (below).
            throws: None,
            rest: parts.rest,
            bounds,
            constraints,
            outer: n,
        };
        // Spellings that differ only in the error-set variables of the function types in them
        // (`throws::vars`) are one type: the variables are unified, and the first one kept.
        let plain = Shape {
            params: shape.params.iter().map(|t| self.plain_type(*t)).collect(),
            ret: self.plain_type(shape.ret),
            ..shape.clone()
        };
        let iface = match self.generic_fns.by_plain.get(&plain).copied() {
            Some(d) => {
                if let Some(m) = self.iface(d).and_then(|i| i.methods.first()) {
                    let had: Vec<TyId> = m.params.iter().map(|p| p.ty).chain([m.ret]).collect();
                    let now: Vec<TyId> = shape.params.iter().copied().chain([shape.ret]).collect();
                    for (a, b) in had.into_iter().zip(now) {
                        self.unify_err_vars(b, a);
                    }
                }
                d
            }
            None => {
                // The errors are no part of the type: its interface's error type is what
                // anything stored into one of its spellings throws, kept as the set of the
                // site of its first spelling (`throws::fn_types`), so the program is checked
                // again when it grows.
                let mut shape = shape;
                shape.throws = parts.site.and_then(|s| self.site_error(s, module));
                let d = self.generic_fn_iface(shape, own_names, module, span);
                if let Some(s) = parts.site {
                    self.generic_fns.sites.insert(d, s);
                }
                self.generic_fns.by_plain.insert(plain, d);
                d
            }
        };
        if let Some(e) = parts.throws.filter(|t| *t != self.ty.never) {
            let e = renumber(self, e);
            let args: Vec<TyId> = (0..n).map(|i| self.ty.param(i as u32)).collect();
            self.add_generic_errors(iface, e, &args, span);
        }
        let args = outer.iter().map(|p| self.ty.param(*p)).collect();
        self.ty.intern(TyKind::Dyn(iface, args))
    }

    /// The interface of generic function shape `s` (made once).
    fn generic_fn_iface(
        &mut self,
        s: Shape,
        own_names: Vec<String>,
        module: usize,
        span: Span,
    ) -> DefId {
        let n = s.outer;
        let mut generics = Generics::default();
        for k in 0..n {
            generics.push(&format!("T{k}"));
        }
        generics.bounds = vec![vec![]; n];
        let mut own = Generics::default();
        for name in &own_names {
            own.push(name);
        }
        own.bounds = s
            .bounds
            .iter()
            .map(|bs| {
                bs.iter()
                    .map(|(iface, args)| Bound {
                        iface: *iface,
                        args: args.clone(),
                    })
                    .collect()
            })
            .collect();
        if s.constraints.iter().any(Option::is_some) {
            own.constraints = s.constraints.clone();
        }
        let n = s.params.len();
        let params = (s.params.iter().enumerate())
            .map(|(k, &ty)| {
                // A call may leave out a rest parameter (the empty array) and an optional one
                // (`undefined`), as for a function value (`body::expr::call`).
                let empty = |kind| crate::hir::Expr { kind, ty, span };
                let default = match self.ty.kind(ty) {
                    _ if s.rest && k + 1 == n => {
                        Some(empty(crate::hir::ExprKind::ArrayLit(vec![])))
                    }
                    TyKind::Option(_, nothing) if nothing.has_undefined() => {
                        Some(empty(crate::hir::ExprKind::Lit(crate::hir::Lit::Null)))
                    }
                    _ => None,
                };
                ParamSig {
                    name: format!("a{k}"),
                    span,
                    ty,
                    mode: crate::collect::sigs::declared_mode(self, ty),
                    default,
                }
            })
            .collect();
        // Always declared (nothing, when the shape has no error type): the generic closures
        // stored into the type are checked against it (`body::expr::generic_values`), so the
        // slot's error type is known before any of them is.
        let throws = Some(crate::defs::DeclaredThrows {
            ty: s.throws,
            span,
            from_body: false,
        });
        let method = IfaceMethod {
            name: CALL.into(),
            span,
            params,
            ret: s.ret,
            mut_this: false,
            default: None,
            is_getter: false,
            generics: own,
            throws,
        };
        let shown = self.show_generic_fn(&s, &own_names);
        let d = self.alloc_def(
            span,
            DefInfo::Iface(Box::new(IfaceInfo {
                name: shown.clone(),
                qual_name: shown,
                module,
                span,
                generics,
                fields: vec![],
                methods: vec![method],
                parents: vec![],
                decl: None,
            })),
        );
        self.generic_fns.made.insert(d, s.rest);
        self.generic_fns.names.insert(d, own_names);
        d
    }

    /// `<T>(a0: T) => T` for shape `s` (its arguments as `T0…`, used only until a use site
    /// shows it with its own: [`Self::display_generic_fn`]).
    fn show_generic_fn(&self, s: &Shape, own: &[String]) -> String {
        let mut names: Vec<String> = (0..s.outer).map(|k| format!("T{k}")).collect();
        names.push("<Self>".into());
        names.extend(own.iter().cloned());
        self.sig_text(&s.params, s.ret, s.rest, own, &names)
    }

    fn sig_text(
        &self,
        params: &[TyId],
        ret: TyId,
        rest: bool,
        own: &[String],
        names: &[String],
    ) -> String {
        let mut ps: Vec<String> = params.iter().map(|t| self.display_in(*t, names)).collect();
        if let (true, Some(last)) = (rest, ps.last_mut()) {
            last.insert_str(0, "...");
        }
        format!(
            "<{}>({}) => {}",
            own.join(", "),
            ps.join(", "),
            self.display_in(ret, names)
        )
    }

    /// Errors `e` (in the terms of the place the interface is used at, its arguments `args`)
    /// are stored into generic function type interface `d`: those its error type lacks are
    /// added to its site's set, and the program is checked again; reported at `span` where
    /// they can't be.
    pub(crate) fn add_generic_errors(&mut self, d: DefId, e: TyId, args: &[TyId], span: Span) {
        let allowed = self
            .iface(d)
            .and_then(|i| i.methods.first())
            .and_then(|m| m.throws)
            .and_then(|t| t.ty);
        let have = match allowed {
            Some(a) => {
                let a = self.subst(a, args);
                self.error_members(a)
            }
            None => vec![],
        };
        let (unit, never) = (self.ty.unit, self.ty.never);
        let extra: Vec<TyId> = (self.error_members(e).into_iter())
            .filter(|m| *m != unit && *m != never && !have.contains(m))
            .collect();
        let Some(extra) = self.error_union(&extra) else {
            return;
        };
        let site: Vec<Span> = self
            .generic_fns
            .sites
            .get(&d)
            .copied()
            .into_iter()
            .collect();
        if !self.seed_generic_sites(&site, extra) {
            let gt = self.ty.intern(TyKind::Dyn(d, args.to_vec()));
            let (e, t) = (self.display(extra), self.display(gt));
            self.error(
                Diagnostic::error(
                    format!("this function throws `{e}`, which `{t}` does not allow"),
                    span,
                )
                .with_note("only `Error` classes are kept as what a generic function type throws; a type the standard library declares throws only what it declares"),
            );
        }
    }

    /// Is `t` a generic function type?
    pub(crate) fn is_generic_fn(&self, t: TyId) -> bool {
        matches!(self.ty.kind(t), TyKind::Dyn(d, _) if self.generic_fns.made.contains_key(d))
    }

    /// The generic function type `t` (an interface value of one made here), as shown with the
    /// type parameter names `names` in scope.
    pub(crate) fn display_generic_fn(&self, t: TyId, names: &[String]) -> Option<String> {
        let TyKind::Dyn(d, args) = self.ty.kind(t) else {
            return None;
        };
        let own = self.generic_fns.names.get(d)?;
        let m = self.iface(*d)?.methods.first()?;
        let mut shown: Vec<String> = args.iter().map(|a| self.display_in(*a, names)).collect();
        shown.push("<Self>".into());
        shown.extend(own.iter().cloned());
        let pts: Vec<TyId> = m.params.iter().map(|p| p.ty).collect();
        let rest = self.generic_fn_rest(*d);
        Some(self.sig_text(&pts, m.ret, rest, own, &shown))
    }

    /// Does generic function type interface `d` take a rest parameter?
    fn generic_fn_rest(&self, d: DefId) -> bool {
        self.generic_fns.made.get(&d).copied().unwrap_or(false)
    }

    /// The signature of generic function type `t`, if it is one.
    pub(crate) fn generic_sig(&self, t: TyId) -> Option<GenericSig> {
        let TyKind::Dyn(d, args) = self.ty.kind(t) else {
            return None;
        };
        self.generic_fns.made.get(d)?;
        let method = self.iface(*d)?.methods.first()?.clone();
        Some(GenericSig {
            iface: *d,
            args: args.clone(),
            method,
            rest: self.generic_fn_rest(*d),
        })
    }
}
