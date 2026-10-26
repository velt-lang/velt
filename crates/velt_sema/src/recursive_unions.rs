//! Recursive union aliases (#1204): `type Json = string | number | boolean | null | Json[] |
//! { [key: string]: Json }`. An alias is expanded where it is used, so one that names itself
//! would expand without end. A union is a compiler-generated enum (`crate::unions`), a
//! definition its own members can name: when a non-generic alias whose body is a union names
//! itself, its union enum is allocated before the body is resolved, the alias's own name
//! stands for it inside the body, and the members found are then given to it. The enum is the
//! canonical union of those members, so the same union written out elsewhere is the same type.
//!
//! The alias may only name itself behind an indirection: as the element of an array, a value of
//! an index signature or a `Map`, or in a function type. Anywhere else (`Json | { next: Json }`,
//! a tuple, or the union itself) the value would contain itself, and the alias stays the error
//! `resolve::expand_alias` reports.

use velt_common::Span;
use velt_syntax::ast;

use crate::ctx::Ctx;
use crate::defs::{DefInfo, EnumInfo, Generics};
use crate::hir::{DefId, Nothing, TyId, TyKind};
use crate::resolve::TyEnv;

impl Ctx<'_> {
    /// The type alias `a` names inside its own body while it is expanded at `args`: its union,
    /// when it is a recursive union alias; for a generic one at type parameters (`SlotContent<T>`
    /// in its own body, or checked on its own), the alias deferred until they are known.
    pub(crate) fn recursive_union_ref(&mut self, a: u32, args: &[TyId], span: Span) -> Option<TyId> {
        if self.aliases[a as usize].decl.generics.is_empty() {
            return self.recursive_unions.get(&a).copied().flatten();
        }
        if !self.is_generic_recursive(a) {
            return None;
        }
        if args.iter().any(|x| self.has_type_params(*x)) {
            return Some(self.deferred_alias(a, args.to_vec(), span));
        }
        self.recursive_union_insts.get(&(a, args.to_vec())).copied()
    }

    /// Is alias `a` generic, with a union body that names it?
    fn is_generic_recursive(&mut self, a: u32) -> bool {
        if let Some(&r) = self.generic_recursive.get(&a) {
            return r;
        }
        let decl = self.aliases[a as usize].decl;
        let r = !decl.generics.is_empty()
            && matches!(decl.ty.kind, ast::TypeExprKind::Union(_))
            && crate::recursive_aliases::names(&decl.ty, &decl.name.name);
        self.generic_recursive.insert(a, r);
        r
    }

    /// Alias `a`'s type at `args` (its enclosing functions' and its own type arguments) when
    /// it is a recursive union alias (module docs), resolved once per list of arguments; `None`
    /// for any other alias. A generic one at type parameters is deferred until they are known.
    pub(crate) fn recursive_union(&mut self, a: u32, args: Vec<TyId>, span: Span) -> Option<TyId> {
        let decl = self.aliases[a as usize].decl;
        let generic = !decl.generics.is_empty();
        if !generic {
            if let Some(&t) = self.recursive_unions.get(&a) {
                return t;
            }
            if !matches!(decl.ty.kind, ast::TypeExprKind::Union(_))
                || !crate::recursive_aliases::names(&decl.ty, &decl.name.name)
            {
                self.recursive_unions.insert(a, None);
                return None;
            }
        } else {
            if !self.is_generic_recursive(a) {
                return None;
            }
            if args.iter().any(|x| self.has_type_params(*x)) {
                return Some(self.deferred_alias(a, args, span));
            }
            if let Some(&t) = self.recursive_union_insts.get(&(a, args.clone())) {
                return Some(t);
            }
        }
        let args = if generic { args } else { vec![] };
        let name = &decl.name.name;
        let ast::TypeExprKind::Union(members) = &decl.ty.kind else {
            return None;
        };
        let nothing = self.members_nothing(a, members, &args);
        let module = self.aliases[a as usize].module;
        let mut qual_name = self.qualify(module, name);
        if generic {
            qual_name = format!("{qual_name}<{}>", self.recursive_union_insts.len());
        }
        let info = EnumInfo {
            name: name.clone(),
            qual_name,
            span: decl.name.span,
            generics: Generics::default(),
            variants: vec![],
            is_union: true,
            decl: None,
        };
        let d = self.alloc_def(decl.name.span, DefInfo::Enum(Box::new(info)));
        let core = self.ty.intern(TyKind::Adt(d, vec![]));
        let own = match nothing {
            Some(n) => self.ty.option_of(core, n),
            None => core,
        };
        self.alias_names.insert(core, name.clone());
        self.alias_names.insert(own, name.clone());
        self.remember_recursive(a, &args, own);
        let body = self.alias_body(a, args.clone(), span);
        if self.adopt_members(d, core, body, nothing) {
            return Some(own);
        }
        let error = self.ty.error;
        self.remember_recursive(a, &args, error);
        let d = velt_common::Diagnostic::error(
            format!("type alias `{name}` refers to itself"),
            decl.name.span,
        )
        .with_note("a union alias may refer to itself behind an array, an index signature, a `Map` or a function type (`type Json = string | Json[] | { [key: string]: Json }`); give any other recursive type a nominal member, such as an interface or a class");
        self.error(d);
        Some(self.ty.error)
    }

    /// Record `t` as alias `a`'s union at `args` (none for a non-generic alias).
    fn remember_recursive(&mut self, a: u32, args: &[TyId], t: TyId) {
        match args.is_empty() {
            true => {
                self.recursive_unions.insert(a, Some(t));
            }
            false => {
                self.recursive_union_insts.insert((a, args.to_vec()), t);
            }
        }
    }

    /// The `null` / `undefined` alias `a`'s union holds at `args`: those written among its
    /// `members`, and those of the members that don't name it (an alias, or a type argument,
    /// that allows `null`).
    fn members_nothing(
        &mut self,
        a: u32,
        members: &[ast::TypeExpr],
        args: &[TyId],
    ) -> Option<Nothing> {
        let mut nothing = written_nothing(members);
        let info = &self.aliases[a as usize];
        let decl = info.decl;
        let module = info.module;
        let names: Vec<String> = (info.outer.iter().cloned())
            .chain(decl.generics.iter().map(|g| g.name.name.clone()))
            .collect();
        let reported = self.diags.len();
        for m in members {
            if crate::recursive_aliases::names(m, &decl.name.name)
                || matches!(m.kind, ast::TypeExprKind::Null | ast::TypeExprKind::Undefined)
            {
                continue;
            }
            let mut env = TyEnv::new(module, &names);
            env.args = args.to_vec();
            let t = self.resolve_type(m, &env);
            let t = match args.is_empty() {
                true => t,
                false => self.subst(t, args),
            };
            if let Some(n) = self.ty.nothing_of(t) {
                nothing = Some(nothing.map_or(n, |x| x.join(n)));
            }
        }
        // Reported where the body is resolved.
        self.diags.truncate(reported);
        nothing
    }

    /// Give union enum `d` the members of `body`, the resolved alias body, when it is a union
    /// with the `nothing` the alias was written with, that holds `core` only behind an
    /// indirection.
    fn adopt_members(
        &mut self,
        d: DefId,
        core: TyId,
        body: TyId,
        nothing: Option<Nothing>,
    ) -> bool {
        let (inner, n) = match *self.ty.kind(body) {
            TyKind::Option(p, n) => (p, Some(n)),
            _ => (body, None),
        };
        let Some((found, args)) = self.union_def(inner) else {
            return false;
        };
        if n != nothing || !args.is_empty() {
            return false;
        }
        let Some(mut variants) = self.enum_info(found).map(|e| e.variants.clone()) else {
            return false;
        };
        // A function type in a member is taken as written, throwing nothing: the error types a
        // function type written without `throws` would infer (`crate::throws::vars`) cannot be
        // solved through a type that holds itself.
        for v in &mut variants {
            v.payload[0] = self.plain_type(v.payload[0]);
        }
        let members: Vec<TyId> = variants.iter().map(|v| v.payload[0]).collect();
        if members.iter().any(|&m| self.inline_in(m, core, 0)) {
            return false;
        }
        if let DefInfo::Enum(e) = &mut self.info[d.0 as usize] {
            e.variants = variants;
        }
        self.unions.insert(members, d);
        true
    }

    /// Does a value of type `t` hold a `core` by value (not behind a heap indirection)?
    fn inline_in(&self, t: TyId, core: TyId, depth: u32) -> bool {
        if t == core {
            return true;
        }
        if depth > 16 {
            return false;
        }
        match self.ty.kind(t) {
            TyKind::Option(x, _) => self.inline_in(*x, core, depth + 1),
            TyKind::Tuple(xs) => xs.iter().any(|x| self.inline_in(*x, core, depth + 1)),
            TyKind::Adt(def, args) => match self.adt(*def) {
                // An object type or a struct holds its fields inline.
                Some(a)
                    if matches!(
                        a.kind,
                        crate::hir::AdtKind::Anon | crate::hir::AdtKind::Struct
                    ) =>
                {
                    a.fields.iter().any(|f| {
                        let ft = match self.ty.kind(f.ty) {
                            TyKind::Param(i) => args.get(*i as usize).copied().unwrap_or(f.ty),
                            _ => f.ty,
                        };
                        self.inline_in(ft, core, depth + 1)
                    })
                }
                Some(_) => false,
                // A union holds its member inline.
                None => self.enum_info(*def).is_some_and(|e| {
                    e.is_union
                        && e.variants.iter().any(|v| {
                            v.payload
                                .iter()
                                .any(|p| self.inline_in(*p, core, depth + 1))
                        })
                }),
            },
            _ => false,
        }
    }
}

/// The `null` / `undefined` written among a union's members: `None` for neither, the
/// three-state `Nothing::Both` for both (#1250).
fn written_nothing(members: &[ast::TypeExpr]) -> Option<Nothing> {
    let null = members
        .iter()
        .any(|m| matches!(m.kind, ast::TypeExprKind::Null));
    let undefined = members
        .iter()
        .any(|m| matches!(m.kind, ast::TypeExprKind::Undefined));
    match (null, undefined) {
        (true, true) => Some(Nothing::Both),
        (true, false) => Some(Nothing::Null),
        (false, true) => Some(Nothing::Undefined),
        (false, false) => None,
    }
}
