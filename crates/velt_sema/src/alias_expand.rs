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
        if info.expanding {
            if let Some(own) = self.recursive_union_ref(a) {
                return own;
            }
            let d = velt_common::Diagnostic::error(
                format!("type alias `{}` refers to itself", decl.name.name),
                t.span,
            )
            .with_note("an alias may refer to itself when it is an object type or an intersection of object types written out (`{ kids: T[] } & { v: number }`) with no field named twice, or a union that names itself behind an array, an index signature, a `Map` or a function type (`type Json = string | Json[]`); give any other recursive type a nominal member, such as an interface or a class");
            self.error(d);
            return self.ty.error;
        }
        let args = self.with_defaults(DefaultsOf::Alias(a), module, &decl.generics, args);
        if args.len() != decl.generics.len() {
            self.arity_error(&decl.name.name, decl.generics.len(), args.len(), t);
            return self.ty.error;
        }
        let args = [outer, args].concat();
        if let Some(own) = self.recursive_union(a, t.span) {
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
        let body = self.resolve_type(&decl.ty, &env);
        let stuck = std::mem::replace(&mut self.type_level.stuck, outer_stuck);
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
