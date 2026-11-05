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
    /// Does `t` hold a `unique symbol` type (`unique`) or a plain `symbol`: itself, or in its
    /// elements, members or fields?
    pub(crate) fn holds_symbol(&mut self, t: TyId, unique: bool, depth: u32) -> bool {
        if self.unique_symbols.is_empty() || depth > 4 {
            return false;
        }
        let found = match unique {
            true => self.unique_symbols.values().any(|&u| u == t),
            false => matches!(self.ty.kind(t), TyKind::Symbol),
        };
        if found {
            return true;
        }
        let parts: Vec<TyId> = match self.ty.kind(t).clone() {
            TyKind::Array(e) | TyKind::Option(e, _) | TyKind::Shared(e) => vec![e],
            TyKind::Promise(e, _) => vec![e],
            TyKind::Map(k, v) => vec![k, v],
            TyKind::Tuple(ts) => ts,
            _ => match self.union_members(t) {
                Some(ms) => ms,
                None if self.is_object_type(t) => {
                    self.public_fields(t).iter().map(|f| f.ty).collect()
                }
                None => vec![],
            },
        };
        parts
            .into_iter()
            .any(|p| self.holds_symbol(p, unique, depth + 1))
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
