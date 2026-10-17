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

impl Ctx<'_> {
    /// The type alias `a` (with no arguments) names inside its own body while it is expanded,
    /// or after: its union, when it is a recursive union alias.
    pub(crate) fn recursive_union_ref(&self, a: u32) -> Option<TyId> {
        self.recursive_unions.get(&a).copied().flatten()
    }

    /// Alias `a`'s type when it is a recursive union alias (module docs), resolved once;
    /// `None` for any other alias.
    pub(crate) fn recursive_union(&mut self, a: u32, span: Span) -> Option<TyId> {
        if let Some(&t) = self.recursive_unions.get(&a) {
            return t;
        }
        let decl = self.aliases[a as usize].decl;
        let name = &decl.name.name;
        let nothing = match &decl.ty.kind {
            ast::TypeExprKind::Union(members)
                if decl.generics.is_empty() && crate::recursive_aliases::names(&decl.ty, name) =>
            {
                written_nothing(members)
            }
            _ => None,
        };
        let Some(nothing) = nothing else {
            self.recursive_unions.insert(a, None);
            return None;
        };
        let module = self.aliases[a as usize].module;
        let info = EnumInfo {
            name: name.clone(),
            qual_name: self.qualify(module, name),
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
        self.recursive_unions.insert(a, Some(own));
        let body = self.alias_body(a, vec![], span);
        if self.adopt_members(d, core, body, nothing) {
            return Some(own);
        }
        self.recursive_unions.insert(a, Some(self.ty.error));
        let d = velt_common::Diagnostic::error(
            format!("type alias `{name}` refers to itself"),
            decl.name.span,
        )
        .with_note("a union alias may refer to itself behind an array, an index signature, a `Map` or a function type (`type Json = string | Json[] | { [key: string]: Json }`); give any other recursive type a nominal member, such as an interface or a class");
        self.error(d);
        Some(self.ty.error)
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
        let Some(variants) = self.enum_info(found).map(|e| e.variants.clone()) else {
            return false;
        };
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

/// The `null` / `undefined` written among a union's members (`Some(None)`: neither); `None`
/// when both are (not supported yet: the alias is left to the usual error).
fn written_nothing(members: &[ast::TypeExpr]) -> Option<Option<Nothing>> {
    let null = members
        .iter()
        .any(|m| matches!(m.kind, ast::TypeExprKind::Null));
    let undefined = members
        .iter()
        .any(|m| matches!(m.kind, ast::TypeExprKind::Undefined));
    match (null, undefined) {
        (true, true) => None,
        (true, false) => Some(Some(Nothing::Null)),
        (false, true) => Some(Some(Nothing::Undefined)),
        (false, false) => Some(None),
    }
}
