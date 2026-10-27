//! Type aliases expanded where they are used: an alias's body at its arguments, deferred
//! (`TyOp::Alias`) while a form in it needs an argument that is still a type parameter (#934),
//! and the type parameters of the functions around an alias declared in a generic body (#350).

use velt_common::Span;
use velt_syntax::ast;

use crate::ctx::Ctx;
use crate::hir::TyId;
use crate::resolve::TyEnv;
use crate::type_defaults::DefaultsOf;

impl Ctx<'_> {
    /// The arguments for the enclosing functions' type parameters an alias declared in a
    /// generic body takes (`AliasInfo::outer`), at a use in `env`; `None` where one isn't known.
    pub(crate) fn outer_alias_args(&mut self, a: u32, env: &TyEnv) -> Option<Vec<TyId>> {
        let outer = self.aliases[a as usize].outer.clone();
        let mut out = vec![];
        for n in &outer {
            let i = env.params.iter().rposition(|p| p == n)?;
            let p = self.ty.param(i as u32);
            out.push(match env.args.is_empty() {
                true => p,
                false => self.subst(p, &env.args),
            });
        }
        Some(out)
    }

    pub(crate) fn expand_alias(&mut self, a: u32, mut args: Vec<TyId>, t: &ast::TypeExpr) -> TyId {
        let info = &self.aliases[a as usize];
        let (module, decl) = (info.module, info.decl);
        let outer: Vec<TyId> = args.drain(..info.outer.len().min(args.len())).collect();
        if self.aliases[a as usize].expanding {
            // With its defaults (`type D<T = number> = string | T | D[]`).
            let own = self.with_defaults(DefaultsOf::Alias(a), module, &decl.generics, args);
            let full = [outer, own].concat();
            if let Some(own) = self.recursive_union_ref(a, &full, t.span) {
                return own;
            }
            let d =
                crate::recursive_aliases::self_reference(&self.alias_blocked, &decl.name, t.span);
            self.error(d);
            return self.ty.error;
        }
        let args = self.with_defaults(DefaultsOf::Alias(a), module, &decl.generics, args);
        if args.len() != decl.generics.len() {
            self.arity_error(&decl.name.name, decl.generics.len(), args.len(), t);
            return self.ty.error;
        }
        let args = [outer, args].concat();
        if let Some(own) = self.recursive_union(a, args.clone(), t.span) {
            return own;
        }
        self.alias_body(a, args, t.span)
    }

    /// The body of alias `a` (with as many `args` as it has parameters) at `args`, used at
    /// `span`. A conditional, mapped, indexed access or template literal type in it that needs
    /// an argument that is still a type parameter makes it the deferred `TyKind::Op` (#934).
    pub(crate) fn alias_body(&mut self, a: u32, args: Vec<TyId>, span: Span) -> TyId {
        let info = &self.aliases[a as usize];
        let decl = info.decl;
        let module = info.module;
        let own = decl.generics.iter().map(|g| g.name.name.clone());
        let names: Vec<String> = info.outer.iter().cloned().chain(own).collect();
        self.aliases[a as usize].used = true;
        self.aliases[a as usize].expanding = true;
        let mut env = TyEnv::new(module, &names);
        env.args = args.clone();
        let outermost = self.type_level.alias_site.is_none();
        if outermost {
            self.type_level.alias_site = Some(span);
        }
        let defer = !self.checking_unused_aliases && args.iter().any(|x| self.has_type_params(*x));
        if defer {
            self.type_level.deferring += 1;
        }
        let outer_stuck = std::mem::replace(&mut self.type_level.stuck, false);
        let reported = self.diags.len();
        let mut body = self.resolve_type(&decl.ty, &env);
        let mut stuck = std::mem::replace(&mut self.type_level.stuck, outer_stuck);
        let calls = call_parts(&decl.ty);
        if stuck && defer && !calls.is_empty() {
            // `((props: F<T>) => R) & { __setup: S<T> }` (a function with properties, #921):
            // only its call signature's parameter types wait for the arguments, so a generic
            // body can build and use the value (`const f = fn as A<T>; f.__setup = …`).
            self.diags.truncate(reported);
            self.type_level.stuck = false;
            let outer = std::mem::replace(&mut self.type_level.callable_params, calls);
            body = self.resolve_type(&decl.ty, &env);
            self.type_level.callable_params = outer;
            stuck = std::mem::replace(&mut self.type_level.stuck, outer_stuck);
        }
        if defer {
            self.type_level.deferring -= 1;
        }
        if outermost {
            self.type_level.alias_site = None;
        }
        self.aliases[a as usize].expanding = false;
        if stuck {
            // Evaluated again, and reported, once the arguments are known.
            self.diags.truncate(reported);
            return self.deferred_alias(a, args, span);
        }
        if names.is_empty() && self.is_structural(body) {
            // `type Shape = { ... } | { ... }`: messages call the union `Shape`.
            self.alias_names
                .entry(body)
                .or_insert_with(|| decl.name.name.clone());
        }
        if self.mixes_nothings(body, &args) {
            self.err(
                "this alias keeps a `P | null` of a type argument that allows `undefined`, whose `undefined` it could not tell from its own `null` yet (#930)",
                span,
            );
            return self.ty.error;
        }
        let t = self.subst(body, &args);
        self.reduce_known_intersection(t)
    }
}

impl Ctx<'_> {
    /// The first alias declared exactly like alias `a` (same name, parameters and body, the
    /// names in them bound to the same items), else `a`: a module's own copy of another's
    /// alias (`type Strip<T> = …` written in both) is one type when deferred, as TypeScript
    /// compares the two structurally.
    pub(crate) fn alias_twin(&mut self, a: u32) -> u32 {
        if let Some(&t) = self.type_level.twins.get(&a) {
            return t;
        }
        let twin = (0..a).find(|&b| self.alias_twins(a, b)).unwrap_or(a);
        self.type_level.twins.insert(a, twin);
        twin
    }

    fn alias_twins(&self, a: u32, b: u32) -> bool {
        let (x, y) = (&self.aliases[a as usize], &self.aliases[b as usize]);
        if x.decl.name.name != y.decl.name.name || !x.outer.is_empty() || !y.outer.is_empty() {
            return false;
        }
        if x.module == y.module {
            return false;
        }
        let gx = crate::type_level::inline::debug_key(&x.decl.generics);
        let gy = crate::type_level::inline::debug_key(&y.decl.generics);
        if gx != gy
            || crate::type_level::inline::debug_key(&x.decl.ty.kind)
                != crate::type_level::inline::debug_key(&y.decl.ty.kind)
        {
            return false;
        }
        let mut names = vec![];
        crate::type_level::inline::names_in(&x.decl.ty, &mut names);
        for g in &x.decl.generics {
            for t in g.bounds.iter().chain(g.default.iter()) {
                crate::type_level::inline::names_in(t, &mut names);
            }
        }
        let (sx, sy) = (&self.scopes[x.module], &self.scopes[y.module]);
        names.iter().all(|n| {
            *n != "*"
                && (sx.items.get(*n) == sy.items.get(*n)
                    || x.decl.generics.iter().any(|g| g.name.name == *n))
        })
    }
}

/// The spans of the function types among the parts of `t` when it is an intersection of them
/// with other parts (a callable object type: `((p: P) => R) & { x: X }`); empty otherwise.
fn call_parts(t: &ast::TypeExpr) -> Vec<Span> {
    let ast::TypeExprKind::Intersection(parts) = &t.kind else {
        return vec![];
    };
    let calls: Vec<Span> = (parts.iter())
        .filter(|p| matches!(p.kind, ast::TypeExprKind::Function { .. }))
        .map(|p| p.span)
        .collect();
    match calls.len() < parts.len() {
        true => calls,
        false => vec![],
    }
}

/// Resolve every type alias that nothing expanded, so its errors are reported too
/// (`type X = Nope;` used nowhere is still an error, as in TypeScript). Aliases that are used
/// were checked at their uses.
pub(crate) fn check_unused_aliases(cx: &mut Ctx) {
    cx.checking_unused_aliases = true;
    for a in 0..cx.aliases.len() {
        if cx.aliases[a].used {
            continue;
        }
        let (module, decl) = (cx.aliases[a].module, cx.aliases[a].decl);
        // A recursive union alias is checked as its union (`crate::recursive_unions`).
        if decl.generics.is_empty()
            && cx.aliases[a].outer.is_empty()
            && cx
                .recursive_union(a as u32, vec![], decl.name.span)
                .is_some()
        {
            continue;
        }
        let mut names: Vec<String> = decl.generics.iter().map(|g| g.name.name.clone()).collect();
        cx.aliases[a].used = true;
        cx.aliases[a].expanding = true;
        // An alias of a generic function type that nothing uses as a type is fine: constants
        // of it are generic functions (`generic_arrows::typed`). Its function type is checked
        // with the type parameters in scope; a use elsewhere reports the form.
        let ty = match &decl.ty.kind {
            ast::TypeExprKind::GenericFunction { type_params, ty } => {
                names.extend(type_params.iter().map(|g| g.name.name.clone()));
                &**ty
            }
            _ => &decl.ty,
        };
        cx.resolve_type(ty, &TyEnv::new(module, &names));
        cx.aliases[a].expanding = false;
    }
    cx.checking_unused_aliases = false;
}
