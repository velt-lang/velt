//! `unique symbol` types (#923, #1254): `typeof K` of a module constant holding a fresh symbol
//! (`const K = Symbol("k")`, `const K: unique symbol = Symbol()`) is a type of its own, which
//! only `K` has, as in TypeScript. It is a brand of `symbol` (`crate::brands`): a field-less
//! definition while bodies are checked, erased to `symbol` before lowering. A `typeof K` value
//! is used wherever a `symbol` is (a `Map` key, a symbol-keyed member, a `symbol` parameter).
//!
//! `K` itself stays a `symbol` value (so `let s = K` can take another symbol later, as
//! TypeScript widens it there); the name `K` converts to `typeof K` where that type is expected
//! (`FnCx::unique_symbol_value`). On a union with a `typeof K` member, `x === K` tests for that
//! member, and the branch where it is false no longer has it (`body::expr::unique_symbol_eq`,
//! the facts in `body::narrow`).

use velt_common::Span;
use velt_syntax::ast;

use crate::ctx::Ctx;
use crate::defs::{AdtInfo, DefInfo, Generics};
use crate::hir::{self, AdtKind, DefId, TyId, TyKind};

impl Ctx<'_> {
    /// `typeof K` of constant `d` when it holds a fresh symbol (module docs), made on first use.
    pub(crate) fn unique_symbol_type(&mut self, d: DefId) -> Option<TyId> {
        if let Some(&t) = self.unique_symbols.get(&d) {
            return Some(t);
        }
        let DefInfo::Global(g) = &self.info[d.0 as usize] else {
            return None;
        };
        let unique = match (g.src.ann, g.src.init) {
            _ if g.mutable => false,
            (Some(ann), _) => is_unique_symbol_type(ann),
            (None, Some(init)) => crate::type_queries::symbol_init(init),
            (None, None) => false,
        };
        if !unique {
            return None;
        }
        let (name, module, span) = (format!("typeof {}", g.name), g.module, g.span);
        let brand = self.alloc_def(span, DefInfo::Adt(Box::new(brand_info(name, module, span))));
        let symbol = self.ty.intern(TyKind::Symbol);
        self.brands.insert(brand, symbol);
        let t = self.ty.intern(TyKind::Adt(brand, vec![]));
        self.unique_symbols.insert(d, t);
        Some(t)
    }

    /// Where `typeof K` of constant `d` is in `t`: `t` itself (or its non-null part), or a
    /// member of its union.
    pub(crate) fn unique_symbol_in(&mut self, t: TyId, d: DefId) -> Option<UniqueAt> {
        let u = *self.unique_symbols.get(&d)?;
        let core = self.ty.opt_payload(t).unwrap_or(t);
        if core == u {
            return Some(UniqueAt::Whole);
        }
        let members = self.union_members(core)?;
        let v = members.iter().position(|&m| m == u)?;
        Some(UniqueAt::Member(v as u32))
    }
}

impl Ctx<'_> {
    /// Is `from` the type `to` with `typeof K` where `to` has `symbol`, as the type itself, a
    /// member of a concrete union (`{ p } | typeof K` and `{ p } | symbol`), or the payload of
    /// a nullable? The two are one layout once brands are erased (`crate::union_layouts`
    /// merges unions of one erased member list), so a `from` value is a `to` as it is.
    pub(crate) fn widens_unique(&mut self, from: TyId, to: TyId) -> bool {
        !self.unique_symbols.is_empty() && from != to && self.unique_widen(from, to, 0)
    }

    fn unique_widen(&mut self, from: TyId, to: TyId, depth: u32) -> bool {
        if from == to {
            return true;
        }
        if depth > 4 {
            return false;
        }
        let symbol = self.ty.intern(TyKind::Symbol);
        if to == symbol {
            return self.unique_symbols.values().any(|&u| u == from);
        }
        if let (TyKind::Option(a, n), TyKind::Option(b, m)) =
            (self.ty.kind(from).clone(), self.ty.kind(to).clone())
        {
            return n == m && self.unique_widen(a, b, depth + 1);
        }
        let (Some((_, fa)), Some((_, ta))) = (self.union_def(from), self.union_def(to)) else {
            return false;
        };
        if !fa.is_empty() || !ta.is_empty() {
            return false;
        }
        let (Some(fm), Some(tm)) = (self.union_members(from), self.union_members(to)) else {
            return false;
        };
        // Each member of either has its counterpart in the other: one erased member list.
        let mut ok = true;
        for &f in &fm {
            ok &= tm.iter().any(|&t| self.unique_widen(f, t, depth + 1));
        }
        for &t in &tm {
            ok &= fm.iter().any(|&f| self.unique_widen(f, t, depth + 1));
        }
        ok
    }
}

impl Ctx<'_> {
    /// `t` with each `unique symbol` type a `symbol`, as it is once brands are erased.
    pub(crate) fn erase_unique(&mut self, t: TyId) -> TyId {
        self.erased(t, false, 0)
    }

    /// Are `a` and `b`, one of them holding a brand, one type once brands are erased?
    pub(crate) fn erases_alike(&mut self, a: TyId, b: TyId) -> bool {
        if self.unique_symbols.is_empty()
            || (self.erase_unique(a) == a && self.erase_unique(b) == b)
        {
            return false;
        }
        // A field-only interface lowers as the object type of its fields.
        self.erased(a, true, 0) == self.erased(b, true, 0)
    }

    /// `h as target` for a union or nullable `target` with a member that erases like `h`
    /// (`{ v: L }` as `{ v: S } | null`): retyped to that member, then converted to `target`.
    pub(crate) fn erased_member_of(&mut self, h: TyId, target: TyId) -> Option<TyId> {
        if self.unique_symbols.is_empty() {
            return None;
        }
        let core = self.ty.core(target).unwrap_or(target);
        let mut members = self.union_members(core).unwrap_or_default();
        if core != target {
            members.push(core);
        }
        (members.into_iter()).find(|&m| m != h && self.erases_alike(h, m))
    }

    fn erased(&mut self, t: TyId, ifaces: bool, depth: u32) -> TyId {
        if self.unique_symbols.is_empty() || depth > 6 {
            return t;
        }
        if self.unique_symbols.values().any(|&u| u == t) {
            return self.ty.intern(TyKind::Symbol);
        }
        let d = depth + 1;
        let go = |cx: &mut Self, x: TyId| cx.erased(x, ifaces, d);
        let kind = match self.ty.kind(t).clone() {
            TyKind::Array(e) => TyKind::Array(go(self, e)),
            TyKind::Option(e, n) => TyKind::Option(go(self, e), n),
            TyKind::Shared(e) => TyKind::Shared(go(self, e)),
            TyKind::Promise(v, e) => TyKind::Promise(go(self, v), e),
            TyKind::Map(k, v) => TyKind::Map(go(self, k), go(self, v)),
            TyKind::Tuple(ts) => TyKind::Tuple(ts.into_iter().map(|e| go(self, e)).collect()),
            TyKind::Adt(a, args) if !args.is_empty() => {
                TyKind::Adt(a, args.into_iter().map(|e| go(self, e)).collect())
            }
            _ => return self.erased_shape(t, ifaces, d),
        };
        self.ty.intern(kind)
    }

    /// [`Self::erased`] for a concrete union or an object type.
    fn erased_shape(&mut self, t: TyId, ifaces: bool, depth: u32) -> TyId {
        if let Some(members) = self.union_members(t) {
            let mut erased: Vec<TyId> = vec![];
            for m in &members {
                let e = self.erased(*m, ifaces, depth);
                if !erased.contains(&e) {
                    erased.push(e);
                }
            }
            return match (erased == members, erased.as_slice()) {
                (true, _) => t,
                (false, [one]) => *one,
                (false, _) => self.union_of(&erased, false, Span::DUMMY),
            };
        }
        let TyKind::Adt(d, _) = *self.ty.kind(t) else {
            return t;
        };
        let (Some(a), true) = (self.adt(d), self.is_object_type(t)) else {
            return t;
        };
        let (module, anon) = (a.module, a.kind == AdtKind::Anon);
        let mut fields = self.public_fields(t);
        let mut changed = ifaces && !anon;
        for f in &mut fields {
            let e = self.erased(f.ty, ifaces, depth);
            changed |= e != f.ty || (ifaces && f.readonly);
            f.ty = e;
            // Only in a comparison (`ifaces`): `readonly` is gone once lowered.
            f.readonly &= !ifaces;
        }
        match changed {
            true => self.anon_type_with(&fields, module),
            false => t,
        }
    }
}

/// Where a `unique symbol` type is in another type.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum UniqueAt {
    /// The type is `typeof K` (or `typeof K | null`).
    Whole,
    /// Member `v` of the union.
    Member(u32),
}

/// The field-less definition of the brand named `name`.
fn brand_info<'m>(name: String, module: usize, span: Span) -> AdtInfo<'m> {
    AdtInfo {
        name: name.clone(),
        qual_name: name,
        kind: AdtKind::Struct,
        module,
        span,
        generics: Generics::default(),
        fields: vec![],
        own_fields_start: 0,
        redefined: vec![],
        base: None,
        methods: Default::default(),
        ctor: None,
        own_ctor: None,
        vtable: vec![],
        vslots: Default::default(),
        implements: vec![],
        has_dispose: false,
        statics: Default::default(),
        decl: None,
    }
}

/// Is `t` the type `unique symbol`?
fn is_unique_symbol_type(t: &ast::TypeExpr) -> bool {
    matches!(&t.kind, ast::TypeExprKind::Named { path, args }
        if args.is_empty() && path.len() == 1 && path[0].name == "unique symbol")
}

impl crate::body::FnCx<'_, '_> {
    /// The module constant holding a fresh symbol that `e` names (no local shadows it).
    pub(crate) fn unique_symbol_named(&mut self, e: &ast::Expr) -> Option<DefId> {
        let id = match &e.kind {
            ast::ExprKind::Ident(i) => i,
            ast::ExprKind::Paren(inner) => return self.unique_symbol_named(inner),
            _ => return None,
        };
        if self.is_local_name(&id.name) {
            return None;
        }
        match self.cx.lookup_item_at(self.module, &id.name, id.span)? {
            crate::ctx::Item::Def(d) => self.cx.unique_symbol_type(d).map(|_| d),
            _ => None,
        }
    }

    /// `h`, a read of a constant `K` holding a fresh symbol, as a `typeof K` where `exp` holds
    /// that type (module docs); `h` itself otherwise.
    pub(crate) fn unique_symbol_value(&mut self, mut h: hir::Expr, exp: TyId) -> hir::Expr {
        if self.cx.unique_symbols.is_empty() {
            return h;
        }
        let hir::ExprKind::Global(d) = h.kind else {
            return h;
        };
        if !matches!(self.cx.ty.kind(h.ty), TyKind::Symbol) {
            return h;
        }
        if self.cx.unique_symbol_in(exp, d).is_some() {
            h.ty = self.cx.unique_symbols[&d];
        }
        h
    }
}
