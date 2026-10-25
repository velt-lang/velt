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
    by_shape: FxHashMap<Shape, DefId>,
    /// Per interface made here: whether its last parameter is a rest parameter.
    made: FxHashMap<DefId, bool>,
    /// The type parameter names each was first written with, for showing it.
    names: FxHashMap<DefId, Vec<String>>,
    /// Where each is written without `throws` in a user module: the sites what its generic
    /// closures throw is added to.
    sites: FxHashMap<DefId, Vec<Span>>,
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
    /// Where it is written without `throws` (a site: `Ctx::resolve_generic_fn`).
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
        // Written without `throws` in a user module: a site, which throws what the generic
        // closures stored into it throw (found by checking again, `throws::fn_types`).
        let user = self.scopes.get(env.module).is_some_and(|s| !s.is_std);
        let site = (written.is_none() && user && span != Span::DUMMY).then_some(span);
        let throws = match site {
            Some(s) => Some(self.site_error(s, env.module).unwrap_or(self.ty.never)),
            None => written,
        };
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
            throws: parts.throws.map(|t| renumber(self, t)),
            rest: parts.rest,
            bounds,
            constraints,
            outer: n,
        };
        let iface = self.generic_fn_iface(shape, own_names, module, span);
        if let Some(s) = parts.site {
            let sites = self.generic_fns.sites.entry(iface).or_default();
            if !sites.contains(&s) {
                sites.push(s);
            }
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
        if let Some(&d) = self.generic_fns.by_shape.get(&s) {
            return d;
        }
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
        let params = s
            .params
            .iter()
            .enumerate()
            .map(|(k, &ty)| ParamSig {
                name: format!("a{k}"),
                span,
                ty,
                mode: crate::collect::sigs::declared_mode(self, ty),
                default: None,
            })
            .collect();
        let throws = s.throws.map(|t| crate::defs::DeclaredThrows {
            ty: (t != self.ty.never).then_some(t),
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
        self.generic_fns.by_shape.insert(s, d);
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

    /// The sites generic function type interface `d` is written at without `throws`.
    pub(crate) fn generic_fn_sites(&self, d: DefId) -> Vec<Span> {
        self.generic_fns.sites.get(&d).cloned().unwrap_or_default()
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
