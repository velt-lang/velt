//! Symbols as values and as member names (docs/reference/types.md "Symbols").
//!
//! A `symbol` is a pointer-sized value compared by identity (`TyKind::Symbol`); `Symbol(d)`,
//! `Symbol.for(k)` and the well-known symbols live in std/symbol.vlt. Velt objects have fixed
//! fields, so a symbol names a member only when the compiler knows which symbol it is: a module
//! constant initialized with `Symbol()`, `Symbol("d")` or `Symbol.for("k")` (TypeScript's
//! `unique symbol`), or a well-known symbol (`[Symbol.iterator]`). Such a member is an ordinary
//! field or method whose name is the symbol's *key name* (`[Symbol(d)]`, the text `console.log`
//! shows for it), so reading it is as fast as reading a named field.
//!
//! A module constant initialized with `Symbol(...)` is a record the compiler emits
//! (`Intrinsic::SymbolStatic`): module constants are evaluated at each use, and every use must be
//! the same symbol, as in JavaScript.

use std::collections::HashMap;

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::ctx::{Ctx, Item};
use crate::defs::DefInfo;
use crate::hir::{self, DefId, ExprKind as H, Intrinsic, TyId, TyKind};

/// The first `SymbolStatic` id of the symbols of module constants (std/symbol.vlt numbers the
/// well-known symbols below it).
const CONSTANT_IDS: u128 = 64;

/// The symbol a module constant holds, known from its initializer.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum KnownSymbol {
    /// `Symbol()` / `Symbol("d")`: the constant's own symbol.
    Own(DefId, Option<String>),
    /// `Symbol.for("k")`: the registry's symbol for `k`, the same in every module.
    Registered(String),
}

/// Key names given out so far: one per symbol, distinct for distinct symbols.
#[derive(Default)]
pub(crate) struct KeyNames {
    names: HashMap<KnownSymbol, String>,
    taken: HashMap<String, KnownSymbol>,
}

/// A string literal argument (`"d"`, or a template without substitutions).
fn literal_text(e: &ast::Expr) -> Option<String> {
    match &e.kind {
        ast::ExprKind::Lit(ast::Lit::Str(s)) => Some(s.clone()),
        ast::ExprKind::Template { quasis, exprs } if exprs.is_empty() => quasis.first().cloned(),
        _ => None,
    }
}

impl Ctx<'_> {
    /// Does `Symbol` in `module` name the global `Symbol` (std/symbol.vlt), not a binding of
    /// the module's own?
    pub(crate) fn names_global_symbol(&self, module: usize) -> bool {
        !self.scopes[module].items.contains_key("Symbol") && self.prelude.contains_key("Symbol")
    }

    /// The symbol module constant `d` holds when its initializer is a call of the global
    /// `Symbol` with constant arguments: `Symbol()`, `Symbol("d")` or `Symbol.for("k")`.
    pub(crate) fn known_symbol(&self, d: DefId) -> Option<KnownSymbol> {
        let g = self.global(d)?;
        let init = g.src.init?;
        if !self.names_global_symbol(g.module) {
            return None;
        }
        let ast::ExprKind::Call { callee, args, .. } = &init.kind else {
            return None;
        };
        match (&callee.kind, args.as_slice()) {
            (ast::ExprKind::Ident(f), []) if f.name == "Symbol" => Some(KnownSymbol::Own(d, None)),
            (ast::ExprKind::Ident(f), [a]) if f.name == "Symbol" => {
                Some(KnownSymbol::Own(d, Some(literal_text(a)?)))
            }
            (ast::ExprKind::Member { object, prop, .. }, [a]) if prop.name == "for" => {
                match &object.kind {
                    ast::ExprKind::Ident(s) if s.name == "Symbol" => {
                        Some(KnownSymbol::Registered(literal_text(a)?))
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// The initializer of module constant `d` when it is `Symbol()` / `Symbol("d")` and its
    /// annotation (if any) is `symbol`: the record the compiler emits for it.
    pub(crate) fn own_symbol_init(&mut self, d: DefId, ann: Option<TyId>) -> Option<hir::Expr> {
        let sym = self.ty.intern(TyKind::Symbol);
        if ann.is_some_and(|t| t != sym) {
            return None;
        }
        let KnownSymbol::Own(_, desc) = self.known_symbol(d)? else {
            return None;
        };
        let span = self.global(d)?.src.init?.span;
        let lit = |l: hir::Lit, ty| hir::Expr {
            kind: H::Lit(l),
            ty,
            span,
        };
        let args = vec![
            lit(hir::Lit::Int(CONSTANT_IDS + d.0 as u128), self.ty.i64),
            lit(
                hir::Lit::Str(desc.clone().unwrap_or_default()),
                self.ty.str_,
            ),
            lit(hir::Lit::Bool(desc.is_some()), self.ty.bool_),
        ];
        Some(hir::Expr {
            kind: H::Call {
                callee: hir::Callee::Intrinsic(Intrinsic::SymbolStatic),
                args,
            },
            ty: sym,
            span,
        })
    }

    /// The member name a computed key `[NAME]` written in `module` stands for (`name` is
    /// `[NAME]`): the key name of the symbol constant `NAME` holds, or the text of a string
    /// constant. Reports an error (and returns `name` itself) otherwise. Other names, including
    /// the well-known symbol keys (`[Symbol.iterator]`), are returned unchanged.
    pub(crate) fn member_key(&mut self, module: usize, name: &str, span: Span) -> String {
        match self.resolve_key(module, name) {
            Ok(n) => n,
            Err(inner) => self.bad_key(name, &inner, span),
        }
    }

    /// [`Self::member_key`] without reporting: `Err(NAME)` for a `[NAME]` that names nothing.
    pub(crate) fn resolve_key(&mut self, module: usize, name: &str) -> Result<String, String> {
        let Some(inner) = name
            .strip_prefix('[')
            .and_then(|n| n.strip_suffix(']'))
            .filter(|n| !n.starts_with("Symbol.") && !n.starts_with("Symbol("))
        else {
            return Ok(name.to_string());
        };
        let d = match self.lookup_item(module, inner) {
            Some(Item::Def(d)) if matches!(self.info[d.0 as usize], DefInfo::Global(_)) => d,
            _ => return Err(inner.to_string()),
        };
        if let Some(sym) = self.known_symbol(d) {
            return Ok(self.key_name(sym));
        }
        match self
            .global(d)
            .and_then(|g| g.src.init)
            .and_then(literal_text)
        {
            Some(text) if !crate::reserved_key(&text) => Ok(text),
            _ => Err(inner.to_string()),
        }
    }

    fn bad_key(&mut self, name: &str, inner: &str, span: Span) -> String {
        self.error(
            Diagnostic::error(
                format!("`{name}` is not a member name the compiler knows: `{inner}` must be a module constant holding a symbol or a string"),
                span,
            )
            .with_note("Velt objects have fixed fields: a computed key names a well-known symbol (`[Symbol.iterator]`), or a module constant initialized with `Symbol(\"...\")`, `Symbol.for(\"...\")` or a string literal")
            .with_note("for keys only known at run time, use a `Map` or a `Record<string, V>`"),
        );
        name.to_string()
    }

    /// The key name of `sym`: `[Symbol(d)]` (`[Symbol()]` without a description), with ` #2`,
    /// ` #3`, … before the `]` when another symbol with that description has the name already.
    pub(crate) fn key_name(&mut self, sym: KnownSymbol) -> String {
        if let Some(n) = self.symbol_keys.names.get(&sym) {
            return n.clone();
        }
        let desc = match &sym {
            KnownSymbol::Own(_, d) => d.clone().unwrap_or_default(),
            KnownSymbol::Registered(k) => k.clone(),
        };
        let mut name = format!("[Symbol({desc})]");
        let mut n = 2;
        while self.symbol_keys.taken.contains_key(&name) {
            name = format!("[Symbol({desc}) #{n}]");
            n += 1;
        }
        self.symbol_keys.taken.insert(name.clone(), sym.clone());
        self.symbol_keys.names.insert(sym, name.clone());
        name
    }
}

/// Is `k` a computed key `[NAME]` the parser built (not a quoted key `"[NAME]"`)? Its span is
/// `NAME` alone, two bytes shorter than its name; a quoted key's spans the quotes too.
pub(crate) fn is_computed_key(k: &ast::Ident) -> bool {
    k.name.starts_with('[')
        && !k.name.starts_with("[Symbol.")
        && (k.span.hi - k.span.lo) as usize + 2 == k.name.len()
}

impl Ctx<'_> {
    /// Does a (non-null) value of type `t` have a member named `name`: a field, a method or an
    /// accessor of its object type, class (or a base class) or interface (or a parent)?
    pub(crate) fn has_member(&self, t: TyId, name: &str) -> bool {
        self.has_member_depth(t, name, 0)
    }

    fn has_member_depth(&self, t: TyId, name: &str, depth: u32) -> bool {
        if depth > 32 {
            return false;
        }
        match self.ty.kind(t) {
            TyKind::Adt(d, _) => match self.adt(*d) {
                Some(a) => {
                    a.fields.iter().any(|f| f.name == name)
                        || a.methods.contains_key(name)
                        || a.base
                            .is_some_and(|b| self.has_member_depth(b, name, depth + 1))
                }
                None => false,
            },
            TyKind::Dyn(d, _) => self.iface_has_member(*d, name, depth),
            _ => false,
        }
    }

    fn iface_has_member(&self, d: DefId, name: &str, depth: u32) -> bool {
        let Some(i) = self.iface(d) else {
            return false;
        };
        depth <= 32
            && (i.fields.iter().any(|f| f.name == name)
                || i.methods.iter().any(|m| m.name == name)
                || i.parents
                    .iter()
                    .any(|p| self.iface_has_member(p.iface, name, depth + 1)))
    }
}
