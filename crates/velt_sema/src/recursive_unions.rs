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
    /// The type alias `a` names inside its own body while it is expanded at `args`: its union,
    /// when it is a recursive union alias; for a generic one at type parameters (`SlotContent<T>`
    /// in its own body, or checked on its own), the alias deferred until they are known.
    pub(crate) fn recursive_union_ref(
        &mut self,
        a: u32,
        args: &[TyId],
        span: Span,
    ) -> Option<TyId> {
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
        // The `null` / `undefined` written among the members; when the resolved body allows
        // more (a member alias such as `JSXElement` that includes `null`, or a type argument
        // that allows it), it is resolved again with those. The diagnostics of the first pass
        // stay (what it cached is not resolved again), the second pass's go.
        let mut nothing = written_nothing(members);
        let mut again = None;
        let mut first = None;
        let (d, core, body) = loop {
            let (d, core, own) = self.new_recursive_union(a, &args, nothing);
            first.get_or_insert(d);
            self.remember_recursive(a, &args, own);
            let body = self.alias_body(a, args.clone(), span);
            if again.is_some() {
                break (d, core, body);
            }
            let found = self.body_nothing(body);
            let wider = match (nothing, found) {
                (_, None) => None,
                (None, Some(f)) => Some(f),
                (Some(n), Some(f)) => (n.join(f) != n).then(|| n.join(f)),
            };
            match wider {
                Some(w) => {
                    nothing = Some(w);
                    again = Some(self.diags.len());
                }
                None => break (d, core, body),
            }
        };
        if let Some(n) = again {
            self.diags.truncate(n);
            // The first pass's union is not used: it keeps no name of the alias's.
            if let Some(DefInfo::Enum(e)) = first.map(|f| &mut self.info[f.0 as usize]) {
                e.qual_name.push_str("<unused>");
            }
        }
        if let Some(other) = self.holds_expanding(a, members) {
            let error = self.ty.error;
            self.remember_recursive(a, &args, error);
            let d = velt_common::Diagnostic::error(
                format!("type alias `{name}` refers to itself"),
                decl.name.span,
            )
            .with_note(format!("it holds `{other}` directly, and `{other}` refers back to it: give one of them a nominal member, such as an interface or a class"));
            self.error(d);
            return Some(self.ty.error);
        }
        if self.adopt_members(d, core, body, nothing) {
            let own = match nothing {
                Some(n) => self.ty.option_of(core, n),
                None => core,
            };
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

    /// The name of another alias that one of alias `a`'s `members` is, written directly (not
    /// behind an indirection), while that alias is itself being expanded: `B` in
    /// `type A = number | B | A[]; type B = string | A | B[]` holds `A`, whose union has no
    /// members yet.
    fn holds_expanding(&self, a: u32, members: &[ast::TypeExpr]) -> Option<String> {
        let module = self.aliases[a as usize].module;
        members.iter().find_map(|m| match &m.kind {
            ast::TypeExprKind::Named { path, .. } if path.len() == 1 => {
                match self.lookup_item(module, &path[0].name) {
                    Some(crate::ctx::Item::Alias(b))
                        if b != a && self.aliases[b as usize].expanding =>
                    {
                        Some(path[0].name.clone())
                    }
                    _ => None,
                }
            }
            _ => None,
        })
    }

    /// The `null` / `undefined` a resolved alias body allows: its own, and those of the type
    /// arguments of the generic union it is (`string | T0 | C[]` at a `T0` of `F | null`).
    fn body_nothing(&self, body: TyId) -> Option<Nothing> {
        let (inner, mut n) = match *self.ty.kind(body) {
            TyKind::Option(p, n) => (p, Some(n)),
            _ => (body, None),
        };
        if let Some((_, args)) = self.union_def(inner) {
            for a in args {
                if let Some(m) = self.ty.nothing_of(a) {
                    n = Some(n.map_or(m, |x| x.join(m)));
                }
            }
        }
        n
    }

    /// A new union enum for alias `a` at `args`: its def, its type, and that type with
    /// `nothing`.
    fn new_recursive_union(
        &mut self,
        a: u32,
        args: &[TyId],
        nothing: Option<Nothing>,
    ) -> (DefId, TyId, TyId) {
        let decl = self.aliases[a as usize].decl;
        let name = &decl.name.name;
        let module = self.aliases[a as usize].module;
        let mut qual_name = self.qualify(module, name);
        if !args.is_empty() {
            qual_name = format!("{qual_name}<{}>", self.info.len());
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
        let top = matches!(self.scopes[module].items.get(name), Some(crate::ctx::Item::Alias(x)) if *x == a);
        if top && args.is_empty() {
            self.gen_binding
                .alias_homes
                .insert(own, (module, name.clone(), false));
            if own != core {
                self.gen_binding
                    .alias_homes
                    .insert(core, (module, name.clone(), true));
            }
        }
        (d, core, own)
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
        let inner = match *self.ty.kind(body) {
            TyKind::Option(p, _) => p,
            _ => body,
        };
        let n = self.body_nothing(body);
        let Some((found, args)) = self.union_def(inner) else {
            return false;
        };
        if n != nothing {
            return false;
        }
        let Some(mut variants) = self.enum_info(found).map(|e| e.variants.clone()) else {
            return false;
        };
        // A union among the members (`C<C<boolean>>`'s `C<boolean>`) is the argument of a
        // generic union: the members are its substituted ones. A function type in a member is
        // taken as written, throwing nothing: the error types a function type written without
        // `throws` would infer (`crate::throws::vars`) cannot be solved through a type that
        // holds itself.
        for v in &mut variants {
            let mut m = v.payload[0];
            if !args.is_empty() {
                m = self.subst(m, &args);
                // A type argument that allows `null` adds its nothing to the union's.
                if let TyKind::Option(p, _) = *self.ty.kind(m) {
                    m = p;
                }
                v.name = self.display(m);
            }
            v.payload[0] = self.plain_type(m);
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
