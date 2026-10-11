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
//! A module constant initialized with `Symbol(...)` or `Symbol.for("k")` is a record the compiler
//! emits (`Intrinsic::SymbolStatic`): module constants are evaluated at each use, and every use
//! must be the same symbol, as in JavaScript (and reading it costs no call). The registry's
//! records (`Symbol.for`) are listed in a table the runtime's registry consults
//! (`Intrinsic::SymbolRegistry`), so `Symbol.for(k)` at run time finds the same record.

use std::collections::HashMap;

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::ctx::{Ctx, Item};
use crate::defs::DefInfo;
use crate::hir::{self, AdtKind, DefId, ExprKind as H, Intrinsic, TyId, TyKind};

/// The first `SymbolStatic` id of the symbols of module constants (std/symbol.vlt numbers the
/// well-known symbols below it).
const CONSTANT_IDS: u128 = 64;

/// The first `SymbolStatic` id of the registry's symbols (`Symbol.for(key)` constants), one per
/// key (`Intrinsic::SymbolStatic`).
pub const REGISTERED_IDS: u128 = 1 << 40;

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
    /// How the source first wrote each key name (`[VNODE]` for `[Symbol(sigx:vnode)]`), for
    /// diagnostics.
    spelled: HashMap<String, String>,
    /// The `SymbolStatic` ids of the registry's symbols, by key.
    registered: HashMap<String, u128>,
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
        self.known_symbol_depth(d, 0)
    }

    fn known_symbol_depth(&self, d: DefId, depth: u32) -> Option<KnownSymbol> {
        let g = self.global(d)?;
        let init = g.src.init?;
        // `const ALIAS = KEY`: the same symbol.
        if let ast::ExprKind::Ident(x) = &init.kind {
            return match self.lookup_item(g.module, &x.name) {
                Some(Item::Def(e)) if depth < 16 => self.known_symbol_depth(e, depth + 1),
                _ => None,
            };
        }
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

    /// The initializer of module constant `d` when it is `Symbol()`, `Symbol("d")` or
    /// `Symbol.for("k")` and its annotation (if any) is `symbol`: the record the compiler emits
    /// for it, so each use of the constant is the same symbol without a call.
    pub(crate) fn symbol_init(&mut self, d: DefId, ann: Option<TyId>) -> Option<hir::Expr> {
        let (id, desc) = match self.known_symbol(d)? {
            KnownSymbol::Own(_, desc) => (CONSTANT_IDS + d.0 as u128, desc),
            KnownSymbol::Registered(key) => {
                let n = self.symbol_keys.registered.len() as u128;
                let id = *self
                    .symbol_keys
                    .registered
                    .entry(key.clone())
                    .or_insert(REGISTERED_IDS + n);
                (id, Some(key))
            }
        };
        // Interned only for a symbol constant: a program without symbols keeps its type ids.
        let sym = self.ty.intern(TyKind::Symbol);
        if ann.is_some_and(|t| t != sym) {
            return None;
        }
        let span = self.global(d)?.src.init?.span;
        let lit = |l: hir::Lit, ty| hir::Expr {
            kind: H::Lit(l),
            ty,
            span,
        };
        let args = vec![
            lit(hir::Lit::Int(id), self.ty.i64),
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

    /// The member name key `k` written in `module` stands for: for a computed key `[NAME]`
    /// ([`is_computed_key`]), the key name of the symbol constant `NAME` holds, or the text of a
    /// string constant; an error is reported (and `k`'s name returned) otherwise. Other names,
    /// including the well-known symbol keys (`[Symbol.iterator]`) and quoted keys (`"[x]"`), are
    /// returned unchanged.
    pub(crate) fn member_key(&mut self, module: usize, k: &ast::Ident) -> String {
        match self.resolve_key(module, k) {
            Ok(n) => {
                self.note_spelling(&n, &k.name);
                n
            }
            Err(inner) => self.bad_key(&k.name, &inner, k.span),
        }
    }

    /// The method-table key (`defs::member_key`) of a method or accessor named `k` in `module`:
    /// a computed key `[KEY]` is resolved as [`Self::member_key`] resolves a field's, so
    /// `o[KEY]`, `o[KEY]()` and `KEY in o` find it. A key that names nothing keeps its name
    /// (reported where the member is declared, [`Self::check_method_key`]).
    pub(crate) fn method_key(&self, module: usize, k: &ast::Ident, is_setter: bool) -> String {
        let name = self
            .resolve_key(module, k)
            .unwrap_or_else(|_| k.name.clone());
        crate::defs::member_key(&name, is_setter)
    }

    /// Reports a method or accessor named by a computed key `[KEY]` that names nothing (see
    /// [`Self::member_key`]).
    pub(crate) fn check_method_key(&mut self, module: usize, k: &ast::Ident) {
        self.member_key(module, k);
    }

    /// Gives every symbol constant of the program its key name, in declaration order, before
    /// any member is collected, so [`Self::resolve_key`] needs no `&mut`.
    pub(crate) fn name_symbol_keys(&mut self) {
        for i in 0..self.info.len() {
            let d = DefId(i as u32);
            if !matches!(self.info[i], DefInfo::Global(_)) {
                continue;
            }
            if let Some(sym) = self.known_symbol(d) {
                self.key_name(sym);
            }
        }
    }

    /// [`Self::member_key`] without reporting: `Err(NAME)` for a `[NAME]` that names nothing.
    pub(crate) fn resolve_key(&self, module: usize, k: &ast::Ident) -> Result<String, String> {
        let name = k.name.as_str();
        let inner = match name.strip_prefix('[').and_then(|n| n.strip_suffix(']')) {
            Some(inner) if is_computed_key(k) => inner,
            _ => return Ok(name.to_string()),
        };
        let d = match self.lookup_item(module, inner) {
            Some(Item::Def(d)) if matches!(self.info[d.0 as usize], DefInfo::Global(_)) => d,
            _ => return Err(inner.to_string()),
        };
        if let Some(sym) = self.known_symbol(d) {
            // Every symbol constant has its name ([`Self::name_symbol_keys`]).
            return match self.symbol_keys.names.get(&sym) {
                Some(key) => Ok(key.clone()),
                None => Err(inner.to_string()),
            };
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

    /// The key name of `sym`: `"\u{FFFF}Symbol(d)"` (`"\u{FFFF}Symbol()"` without a
    /// description), with ` #2`, ` #3`, … after it when another symbol with that description has
    /// the name already (`property_order::symbol_key_name`; no written key starts with U+FFFF).
    pub(crate) fn key_name(&mut self, sym: KnownSymbol) -> String {
        if let Some(n) = self.symbol_keys.names.get(&sym) {
            return n.clone();
        }
        let desc = match &sym {
            KnownSymbol::Own(_, d) => d.clone().unwrap_or_default(),
            KnownSymbol::Registered(k) => k.clone(),
        };
        let mut n = 1;
        let mut name = crate::property_order::symbol_key_name(&desc, n);
        while self.symbol_keys.taken.contains_key(&name) {
            n += 1;
            name = crate::property_order::symbol_key_name(&desc, n);
        }
        self.symbol_keys.taken.insert(name.clone(), sym.clone());
        self.symbol_keys.names.insert(sym, name.clone());
        name
    }
}

impl Ctx<'_> {
    /// Records that the source wrote key name `key` as `spelling` (`[KEY]`), if it is the first.
    pub(crate) fn note_spelling(&mut self, key: &str, spelling: &str) {
        if key != spelling {
            let spelled = &mut self.symbol_keys.spelled;
            spelled
                .entry(key.to_string())
                .or_insert_with(|| spelling.to_string());
        }
    }

    /// Member name `name` as the source writes it: `[VNODE]` for the key name of the symbol
    /// constant `VNODE` holds (`[Symbol(d)]` if no key wrote it), `[Symbol.iterator]` for a
    /// well-known symbol; `None` for a name that is not a symbol key.
    pub(crate) fn key_spelling(&self, name: &str) -> Option<String> {
        if let Some(s) = self.symbol_keys.spelled.get(name) {
            return Some(s.clone());
        }
        if name.starts_with("[Symbol.") {
            return Some(name.to_string());
        }
        self.symbol_keys
            .taken
            .contains_key(name)
            .then(|| crate::property_order::symbol_key(name))
            .flatten()
    }

    /// Member name `name` for a diagnostic: [`Self::key_spelling`] for a symbol key, else the
    /// name itself.
    pub(crate) fn shown_name(&self, name: &str) -> String {
        self.key_spelling(name).unwrap_or_else(|| name.to_string())
    }
}

/// Is `k` a computed key `[NAME]` the parser built (not a quoted key `"[NAME]"`)? Its span is
/// `NAME` alone, two bytes shorter than its name; a quoted key's spans the quotes too.
pub(crate) fn is_computed_key(k: &ast::Ident) -> bool {
    k.name.starts_with('[')
        && !k.name.starts_with("[Symbol.")
        && (k.span.hi - k.span.lo) as usize + 2 == k.name.len()
}

/// Whether a value has a member, as `KEY in o` tests it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Presence {
    /// Always (a required field, a method or an accessor; a class's optional field, which
    /// TypeScript-compiled classes define).
    Yes,
    /// Never.
    No,
    /// While its optional field is present (`[KEY]?: T` in an object type or interface): not
    /// null, or for `[KEY]?: T | null` in an object type its presence flag (`true`).
    Optional(bool),
}

impl Ctx<'_> {
    /// Whether a (non-null) value of a type the program declares, `t`, has a member named
    /// `name`: a field, a method or an accessor of its object type, class (or a base class) or
    /// interface (or a parent). `None` for other types (built-in and std ones).
    pub(crate) fn declared_presence(&self, t: TyId, name: &str) -> Option<Presence> {
        self.presence_depth(t, name, 0)
    }

    fn presence_depth(&self, t: TyId, name: &str, depth: u32) -> Option<Presence> {
        if depth > 32 {
            return Some(Presence::No);
        }
        match self.ty.kind(t) {
            TyKind::Adt(d, _) => {
                let a = self.adt(*d)?;
                if a.kind != AdtKind::Anon && self.scopes[a.module].is_std {
                    return None;
                }
                if let Some(f) = a.fields.iter().find(|f| f.name == name) {
                    return Some(match f.optional && a.kind != AdtKind::Class {
                        true => Presence::Optional(crate::anon::has_presence(&self.ty, a.kind, f)),
                        false => Presence::Yes,
                    });
                }
                if a.methods.contains_key(name) {
                    return Some(Presence::Yes);
                }
                match a.base {
                    Some(b) => self.presence_depth(b, name, depth + 1),
                    None => Some(Presence::No),
                }
            }
            TyKind::Dyn(d, _) => {
                let i = self.iface(*d)?;
                if self.scopes[i.module].is_std {
                    return None;
                }
                self.iface_presence(*d, name, depth)
            }
            _ => None,
        }
    }

    fn iface_presence(&self, d: DefId, name: &str, depth: u32) -> Option<Presence> {
        let i = self.iface(d)?;
        if depth > 32 {
            return Some(Presence::No);
        }
        if let Some(f) = i.fields.iter().find(|f| f.name == name) {
            return Some(match f.optional {
                true => Presence::Optional(false),
                false => Presence::Yes,
            });
        }
        if i.methods.iter().any(|m| m.name == name) {
            return Some(Presence::Yes);
        }
        for p in &i.parents {
            match self.iface_presence(p.iface, name, depth + 1) {
                Some(Presence::No) | None => {}
                found => return found,
            }
        }
        Some(Presence::No)
    }
}
