//! Type-parameter defaults (`class Box<T, E = never>`, `type Pair<A, B = A>`,
//! `function f<T = string>()`): checked once, at the declaration, and stored resolved, in terms
//! of the parameters before each one (a function's in its `Generics::defaults`).
//!
//! As in TypeScript, a default may only mention the parameters declared before it (not itself,
//! not a later one), and must not need its own declaration's defaults again (`class S<T = S>`:
//! a circular default). [`check_all`] resolves every declaration's defaults after phase 1 of
//! collection, so an unknown type in a default is reported there, once, even when no use
//! leaves the parameter out.

use std::collections::{HashMap, HashSet};

use velt_common::Diagnostic;
use velt_syntax::ast;

use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::hir::{DefId, TyId};
use crate::resolve::TyEnv;

/// Whose type parameters: a class, struct or interface, or a type alias (its index).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum DefaultsOf {
    Def(DefId),
    Alias(u32),
}

/// The resolved defaults, and the ones being resolved.
#[derive(Default)]
pub(crate) struct TypeDefaults {
    /// Per declaration, per parameter: its default (`Error` when invalid), `None` without one.
    resolved: HashMap<DefaultsOf, Vec<Option<TyId>>>,
    /// The declarations whose defaults are being resolved, with the parameter at hand.
    resolving: Vec<(DefaultsOf, usize, ast::Ident)>,
    /// Declarations reported for a circular default.
    circular: HashSet<DefaultsOf>,
    /// Circular defaults found so far (a default resolved while one was found is invalid too).
    hits: u32,
}

/// Resolve the defaults of every declaration that has some (module docs).
pub(crate) fn check_all(cx: &mut Ctx) {
    let mut decls = vec![];
    for (i, info) in cx.info.iter().enumerate() {
        let (module, gs) = match info {
            DefInfo::Adt(a) => match a.decl {
                Some(x) => (a.module, &x.generics[..]),
                None => continue,
            },
            DefInfo::Iface(i) => match i.decl {
                Some(x) => (i.module, &x.generics[..]),
                None => continue,
            },
            _ => continue,
        };
        if gs.iter().any(|g| g.default.is_some()) {
            decls.push((DefaultsOf::Def(DefId(i as u32)), module, gs));
        }
    }
    for (a, info) in cx.aliases.iter().enumerate() {
        let gs = &info.decl.generics[..];
        if gs.iter().any(|g| g.default.is_some()) {
            decls.push((DefaultsOf::Alias(a as u32), info.module, gs));
        }
    }
    for (key, module, gs) in decls {
        cx.param_defaults(key, module, gs);
    }
}

impl Ctx<'_> {
    /// `args` completed with the defaults of the parameters `gs` (of `key`) they leave out.
    /// Unchanged when a missing parameter has no default (the caller reports the arity).
    pub(crate) fn with_defaults(
        &mut self,
        key: DefaultsOf,
        module: usize,
        gs: &[ast::GenericParam],
        mut args: Vec<TyId>,
    ) -> Vec<TyId> {
        if args.len() >= gs.len() || gs[args.len()..].iter().any(|g| g.default.is_none()) {
            return args;
        }
        let defaults = self.param_defaults(key, module, gs);
        for i in args.len()..gs.len() {
            let t = match &defaults {
                Some(d) => d[i].unwrap_or(self.ty.error),
                None => self.ty.error,
            };
            let t = self.subst(t, &args);
            args.push(t);
        }
        args
    }

    /// The defaults of class `d`'s type parameters (in terms of the ones before each), for
    /// slots a `new` leaves to inference.
    pub(crate) fn adt_param_defaults(&mut self, d: DefId) -> Vec<Option<TyId>> {
        let decl = match &self.info[d.0 as usize] {
            DefInfo::Adt(a) => a.decl.map(|x| (a.module, &x.generics[..])),
            _ => None,
        };
        let Some((module, gs)) = decl else {
            return vec![];
        };
        if gs.iter().all(|g| g.default.is_none()) {
            return vec![];
        }
        self.param_defaults(DefaultsOf::Def(d), module, gs)
            .unwrap_or_default()
    }

    /// The defaults of `gs` (module docs); `None` while they are being resolved (a circular
    /// default, reported here).
    fn param_defaults(
        &mut self,
        key: DefaultsOf,
        module: usize,
        gs: &[ast::GenericParam],
    ) -> Option<Vec<Option<TyId>>> {
        if let Some(d) = self.type_defaults.resolved.get(&key) {
            return Some(d.clone());
        }
        let td = &mut self.type_defaults;
        if let Some(at) = td.resolving.iter().position(|r| r.0 == key) {
            // Every parameter on the way back to this declaration is part of the cycle.
            td.hits += 1;
            let cycle: Vec<ast::Ident> = td.resolving[at..]
                .iter()
                .filter(|r| td.circular.insert(r.0))
                .map(|r| r.2.clone())
                .collect();
            for g in cycle {
                self.error(
                    Diagnostic::error(
                        format!("type parameter `{}` has a circular default", g.name),
                        g.span,
                    )
                    .with_note(
                        "its default needs the defaults of its own declaration again: write the type arguments out in the default",
                    ),
                );
            }
            return None;
        }
        let names: Vec<String> = gs.iter().map(|g| g.name.name.clone()).collect();
        let env = TyEnv::new(module, &names);
        let mut out = vec![];
        for (i, g) in gs.iter().enumerate() {
            let Some(d) = &g.default else {
                out.push(None);
                continue;
            };
            if !self.declared_before(d, &gs[i..]) {
                out.push(Some(self.ty.error));
                continue;
            }
            self.type_defaults.resolving.push((key, i, g.name.clone()));
            let hits = self.type_defaults.hits;
            let t = self.resolve_type(d, &env);
            self.type_defaults.resolving.pop();
            let t = match self.type_defaults.hits == hits {
                true => t,
                false => self.ty.error,
            };
            out.push(Some(t));
        }
        self.type_defaults.resolved.insert(key, out.clone());
        Some(out)
    }

    /// The defaults of a function's own type parameters `gs` (`function f<T, U = T[]>()`),
    /// resolved in `env` (the owner's parameters, then the function's): per parameter, `None`
    /// without one, `Error` when invalid (reported).
    pub(crate) fn fn_param_defaults(
        &mut self,
        gs: &[ast::GenericParam],
        env: &TyEnv,
    ) -> Vec<Option<TyId>> {
        let mut out = vec![];
        for (i, g) in gs.iter().enumerate() {
            let t = match &g.default {
                None => None,
                Some(d) if !self.declared_before(d, &gs[i..]) => Some(self.ty.error),
                Some(d) => Some(self.resolve_type(d, env)),
            };
            out.push(t);
        }
        out
    }

    /// Does default `d` of the first of `rest` use only the parameters declared before it?
    /// Reported when not.
    fn declared_before(&mut self, d: &ast::TypeExpr, rest: &[ast::GenericParam]) -> bool {
        let Some(bad) = not_declared_before(d, rest) else {
            return true;
        };
        let g = &rest[0];
        let msg = match bad.name == g.name.name {
            true => format!(
                "the default of type parameter `{}` refers to itself",
                g.name.name
            ),
            false => format!(
                "the default of type parameter `{}` refers to `{}`, which is declared after it",
                g.name.name, bad.name
            ),
        };
        self.error(Diagnostic::error(msg, bad.span).with_note(
            "a default may only use the type parameters declared before it (as in TypeScript)",
        ));
        false
    }
}

/// The first name in default `t` of the first of `rest` that is one of `rest` (the parameter
/// itself and the ones after it).
fn not_declared_before<'a>(
    t: &'a ast::TypeExpr,
    rest: &[ast::GenericParam],
) -> Option<&'a ast::Ident> {
    use ast::TypeExprKind as K;
    match &t.kind {
        K::Named { path, args } => {
            if let [id] = path.as_slice() {
                if rest.iter().any(|g| g.name.name == id.name) {
                    return Some(id);
                }
            }
            args.iter().find_map(|a| not_declared_before(a, rest))
        }
        K::Array(e) => not_declared_before(e, rest),
        K::Tuple(ts) | K::Union(ts) | K::Intersection(ts) => {
            ts.iter().find_map(|a| not_declared_before(a, rest))
        }
        K::Indexed { object, key } => {
            not_declared_before(object, rest).or_else(|| not_declared_before(key, rest))
        }
        K::Function {
            params,
            ret,
            throws,
        } => params
            .iter()
            .chain(std::iter::once(&**ret))
            .chain(throws.as_deref())
            .find_map(|a| not_declared_before(a, rest)),
        K::Object(fields) => fields.iter().find_map(|f| not_declared_before(&f.ty, rest)),
        K::Predicate { ty, .. } => ty.as_deref().and_then(|a| not_declared_before(a, rest)),
        K::Literal(_) | K::Null | K::Void => None,
    }
}
