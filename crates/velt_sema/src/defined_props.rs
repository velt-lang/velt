//! Symbol brands added with `Object.defineProperty(obj, KEY, { value })` (#922), found before
//! the declarations are collected. Velt objects have the fields their type declares, so the
//! brand becomes an optional field `[KEY]?: T` of the type `obj` is declared with (an interface
//! or an object type alias of the program: `const cell: Cell = { ... }`, or `{ ... } as Cell`);
//! `KEY in v` then tests it on any `Cell`, as JavaScript finds the property on the objects that
//! have it. `T` is the type of the value, a literal (`true`, `null`, a string or a number).
//!
//! A property `defineProperty` makes is not enumerable unless the descriptor says
//! `enumerable: true`, and not writable unless it says `writable: true`. Velt keeps both per
//! symbol: a symbol defined this way names a hidden field everywhere (`hir::FieldDef::hidden`:
//! `console.log` and spreads leave it out, as Node does), so writing a property with that symbol
//! elsewhere (`{ [KEY]: v }`, `o[KEY] = v`), which would make an enumerable property, or a
//! writable one when JavaScript would throw, is an error at the `defineProperty` call
//! (`object_builtins.rs`). Programs without such a call are not rewritten.

use velt_syntax::ast::{self, ExprKind as E};
use velt_syntax::visit::{self, Visit};

use rustc_hash::FxHashSet;

use velt_common::Span;

use crate::ctx::Ctx;
use crate::SourceModule;

/// One `Object.defineProperty(obj, KEY, { ... })` call with a computed key.
#[derive(Clone, Debug)]
pub(crate) struct Brand {
    /// The module of the call, which resolves `KEY`.
    pub module: usize,
    /// The key as written (`CELL`).
    pub key: ast::Ident,
    /// The descriptor's `enumerable: true`.
    pub enumerable: bool,
    /// The descriptor's `writable: true`.
    pub writable: bool,
    /// The descriptor's `configurable: true`.
    pub configurable: bool,
}

/// The brands of a program, and the keys it also writes directly.
#[derive(Clone, Debug, Default)]
pub(crate) struct Brands {
    pub sites: Vec<Brand>,
    /// The module and key of each `{ [KEY]: v }` and `o[KEY] = v` with a name as the key.
    pub direct: Vec<(usize, ast::Ident)>,
}

/// The brands of `modules` (the program's own; std defines none), and the modules with each
/// brand's field added to the type it is defined on (`None`: nothing to add).
pub(crate) fn rewrite(modules: &[SourceModule]) -> (Brands, Option<Vec<SourceModule>>) {
    let mut brands = Brands::default();
    // (type name, field, written type): the members to add.
    let mut adds: Vec<(String, ast::Ident, ast::TypeExpr)> = vec![];
    // (module, key) of each symbol member an asserted literal adds.
    let mut cast_keys: Vec<(usize, String)> = vec![];
    for (i, m) in modules.iter().enumerate() {
        if m.is_std {
            continue;
        }
        let mut scan = Scan {
            module: i,
            declared: vec![],
            strings: FxHashSet::default(),
            asserted: vec![],
            brands: &mut brands,
            adds: &mut adds,
        };
        visit::walk_module(&m.ast, &mut scan);
        // A string constant's key names a string field, which the type declares or not; a key
        // that is not a module constant is reported where the literal is.
        let (strings, asserted) = (scan.strings, scan.asserted);
        let consts = module_consts(&m.ast);
        for a in asserted {
            let key = &a.1.name[1..a.1.name.len() - 1];
            if strings.contains(key) || !consts.contains(key) {
                continue;
            }
            cast_keys.push((i, key.to_string()));
            // Literals with different types for one member make it an `any`.
            let same = |b: &(String, ast::Ident, ast::TypeExpr)| b.0 == a.0 && b.1.name == a.1.name;
            if let Some(b) = adds.iter_mut().find(|b| same(b)) {
                if type_text(&b.2) != type_text(&a.2) {
                    b.2 = named_type("any", b.2.span);
                }
                continue;
            }
            adds.push(a);
        }
    }
    if adds.is_empty() {
        return (brands, None);
    }
    let mut out: Vec<SourceModule> = modules
        .iter()
        .map(|m| SourceModule {
            path: m.path.clone(),
            is_std: m.is_std,
            file: m.file,
            ast: m.ast.clone(),
            imports: m.imports.clone(),
            jsx_runtime: m.jsx_runtime.clone(),
        })
        .collect();
    for m in out.iter_mut().filter(|m| !m.is_std) {
        for item in &mut m.ast.items {
            add_members(item, &adds);
        }
    }
    for (i, m) in out.iter_mut().enumerate() {
        let keys: Vec<&str> = (brands.sites.iter())
            .filter(|b| b.module == i)
            .map(|b| b.key.name.as_str())
            .filter(|k| adds.iter().any(|a| a.1.name == format!("[{k}]")))
            .chain(cast_keys.iter().filter(|c| c.0 == i).map(|c| c.1.as_str()))
            .collect();
        hoist_keys(&mut m.ast, &keys);
    }
    (brands, Some(out))
}

/// Move the `const KEY = Symbol(...)` of each of `keys` that the parser moved into a script's
/// generated `main` (only top-level statements used it) back to module level: a field the
/// rewrite added now names it, and a computed key names a module constant (the parser keeps
/// such a constant there too). Making a symbol has no effects, so it may run first.
fn hoist_keys(module: &mut ast::Module, keys: &[&str]) {
    if keys.is_empty() {
        return;
    }
    let Some(main) = module.items.iter_mut().find_map(|i| match &mut i.kind {
        ast::ItemKind::Function(f)
            if f.sig.name.name == "main" && f.sig.name.span.lo == f.sig.name.span.hi =>
        {
            Some(f)
        }
        _ => None,
    }) else {
        return;
    };
    let mut hoisted = vec![];
    main.body.stmts.retain(|s| {
        let ast::StmtKind::Var(v) = &s.kind else {
            return true;
        };
        let makes_symbol = matches!(&v.init, Some(ast::Expr { kind: E::Call { callee, .. }, .. })
            if matches!(&callee.kind, E::Ident(c) if c.name == "Symbol"));
        let named = matches!(&v.pattern.kind, ast::PatternKind::Ident(n) if keys.contains(&n.name.as_str()));
        if v.kind == ast::VarKind::Const && makes_symbol && named {
            hoisted.push(ast::Item {
                kind: ast::ItemKind::Var(v.clone()),
                exported: false,
                span: s.span,
            });
            return false;
        }
        true
    });
    module.items.extend(hoisted);
}

/// Add the brand members `adds` names to `item`, if it declares one of their types, or to the
/// declarations nested in its body (a script's top level is a function's body).
fn add_members(item: &mut ast::Item, adds: &[(String, ast::Ident, ast::TypeExpr)]) {
    match &mut item.kind {
        ast::ItemKind::Function(f) => add_nested(&mut f.body.stmts, adds),
        ast::ItemKind::Interface(i) => {
            for (_, field, t) in adds.iter().filter(|a| a.0 == i.name.name) {
                if i.fields.iter().any(|f| f.name.name == field.name) {
                    continue;
                }
                let span = i.name.span;
                i.fields.push(ast::Field {
                    name: field.clone(),
                    ty: t.clone(),
                    default: None,
                    readonly: false,
                    optional: true,
                    is_private: false,
                    is_static: false,
                    span,
                });
            }
        }
        ast::ItemKind::TypeAlias(a) => {
            let ast::TypeExprKind::Object(fields) = &mut a.ty.kind else {
                return;
            };
            for (_, field, t) in adds.iter().filter(|x| x.0 == a.name.name) {
                if fields.iter().any(|f| f.name.name == field.name) {
                    continue;
                }
                let span = a.name.span;
                let null = ast::TypeExpr {
                    kind: ast::TypeExprKind::Null,
                    span,
                };
                fields.push(ast::ObjectTypeField {
                    name: field.clone(),
                    ty: ast::TypeExpr {
                        kind: ast::TypeExprKind::Union(vec![t.clone(), null]),
                        span,
                    },
                    optional: true,
                    readonly: false,
                    span,
                });
            }
        }
        _ => {}
    }
}

struct Scan<'b> {
    module: usize,
    /// Variables declared with a named type (`const cell: Cell = …`, `… as Cell`): name and
    /// type name, in source order (the last one wins).
    declared: Vec<(String, String)>,
    /// The constants initialized with a string (`const NAME = "name"`).
    strings: FxHashSet<String>,
    /// The members asserted literals add ([`Scan::cast`]), before the string keys are dropped.
    asserted: Vec<(String, ast::Ident, ast::TypeExpr)>,
    brands: &'b mut Brands,
    adds: &'b mut Vec<(String, ast::Ident, ast::TypeExpr)>,
}

impl<'a> Visit<'a> for Scan<'_> {
    fn var_decl(&mut self, v: &'a ast::VarDecl) {
        let ast::PatternKind::Ident(name) = &v.pattern.kind else {
            return;
        };
        if matches!(
            v.init.as_ref().map(|e| &e.kind),
            Some(E::Lit(ast::Lit::Str(_)) | E::Template { .. })
        ) {
            self.strings.insert(name.name.clone());
        }
        let written = v.ty.as_ref().or(match v.init.as_ref().map(|e| &e.kind) {
            Some(E::Cast { ty, .. }) => Some(ty),
            _ => None,
        });
        if let Some(t) = written.and_then(type_name) {
            self.declared.push((name.name.clone(), t));
        }
    }

    fn expr(&mut self, e: &'a ast::Expr) {
        match &e.kind {
            E::Call { callee, args, .. } => self.call(callee, args),
            E::Assign {
                target, op: None, ..
            } => {
                if let E::Index { index, .. } = &target.kind {
                    if let E::Ident(k) = &index.kind {
                        self.brands.direct.push((self.module, k.clone()));
                    }
                }
            }
            E::Cast { expr, ty } => self.cast(expr, ty),
            E::Object(props) => {
                for p in props {
                    if let ast::ObjectProp::KeyValue(k, _) = p {
                        if let Some(inner) = computed(&k.name) {
                            let key = ast::Ident {
                                name: inner.to_string(),
                                span: k.span,
                            };
                            self.brands.direct.push((self.module, key));
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

impl Scan<'_> {
    /// `{ ..., [KEY]: v } as T` (#923): TypeScript allows members `T` doesn't declare in an
    /// asserted literal, and the object keeps them, so a symbol-keyed member with a literal
    /// value becomes the optional field `[KEY]?: V` of `T`, as for a `defineProperty` brand
    /// (enumerable, as a literal's members are). Nothing is added when `T` declares the key.
    fn cast(&mut self, expr: &ast::Expr, ty: &ast::TypeExpr) {
        let mut inner = expr;
        while let E::Paren(e) = &inner.kind {
            inner = e;
        }
        let (E::Object(props), Some(type_name)) = (&inner.kind, type_name(ty)) else {
            return;
        };
        for p in props {
            let ast::ObjectProp::KeyValue(k, v) = p else {
                continue;
            };
            let Some(key) = computed(&k.name) else {
                continue;
            };
            // Another value is an `any` (only `any` and `in` see the member).
            let t = literal_type(v).unwrap_or_else(|| named_type("any", v.span));
            if !key
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '$')
            {
                continue;
            }
            let field = ast::Ident {
                name: k.name.clone(),
                span: k.span,
            };
            self.asserted.push((type_name.clone(), field, t));
        }
    }

    /// `Object.defineProperty(obj, KEY, { ... })`.
    fn call(&mut self, callee: &ast::Expr, args: &[ast::Expr]) {
        let E::Member { object, prop, .. } = &callee.kind else {
            return;
        };
        let (E::Ident(ns), [obj, key, desc]) = (&object.kind, args) else {
            return;
        };
        if ns.name != "Object" || prop.name != "defineProperty" {
            return;
        }
        let (E::Ident(k), E::Object(props)) = (&key.kind, &desc.kind) else {
            return;
        };
        let flag = |name: &str| {
            props.iter().any(|p| {
                matches!(p, ast::ObjectProp::KeyValue(n, v)
                    if n.name == name && matches!(v.kind, E::Lit(ast::Lit::Bool(true))))
            })
        };
        self.brands.sites.push(Brand {
            module: self.module,
            key: k.clone(),
            enumerable: flag("enumerable"),
            writable: flag("writable"),
            configurable: flag("configurable"),
        });
        // The value's type, for the field the type gets.
        let value = props.iter().find_map(|p| match p {
            ast::ObjectProp::KeyValue(n, v) if n.name == "value" => Some(v),
            _ => None,
        });
        let (E::Ident(o), Some(value)) = (&obj.kind, value) else {
            return;
        };
        let Some(ty) = literal_type(value) else {
            return;
        };
        let declared = self.declared.iter().rev().find(|(n, _)| *n == o.name);
        if let Some((_, type_name)) = declared {
            // `[KEY]` as the parser writes a computed key: its span is `KEY` alone
            // (`symbols::is_computed_key`).
            let field = computed_key(k);
            self.adds.push((type_name.clone(), field, ty));
        }
    }
}

/// The names of `module`'s constants and imports: the names a computed key may resolve to
/// (a script's top-level constants are in its generated `main`).
fn module_consts(module: &ast::Module) -> FxHashSet<String> {
    let mut names = FxHashSet::default();
    let mut add = |v: &ast::VarDecl| {
        if let (ast::VarKind::Const, ast::PatternKind::Ident(n)) = (v.kind, &v.pattern.kind) {
            names.insert(n.name.clone());
        }
    };
    let mut imported = vec![];
    for item in &module.items {
        match &item.kind {
            ast::ItemKind::Var(v) => add(v),
            ast::ItemKind::Function(f)
                if f.sig.name.name == "main" && f.sig.name.span.lo == f.sig.name.span.hi =>
            {
                for s in &f.body.stmts {
                    if let ast::StmtKind::Var(v) = &s.kind {
                        add(v);
                    }
                }
            }
            ast::ItemKind::Import(i) => {
                imported.extend(i.names.iter().map(|n| n.alias.as_ref().unwrap_or(&n.name)));
            }
            _ => {}
        }
    }
    names.extend(imported.into_iter().map(|n| n.name.clone()));
    names
}

/// [`add_members`] for the declarations among `stmts` (and in their blocks).
fn add_nested(stmts: &mut [ast::Stmt], adds: &[(String, ast::Ident, ast::TypeExpr)]) {
    for s in stmts {
        match &mut s.kind {
            ast::StmtKind::Item(item) => add_members(item, adds),
            ast::StmtKind::Block(b) => add_nested(&mut b.stmts, adds),
            _ => {}
        }
    }
}

/// The name of a named type (`Cell`, `Cell<T>`, `ns.Cell`: its last segment).
fn type_name(t: &ast::TypeExpr) -> Option<String> {
    match &t.kind {
        ast::TypeExprKind::Named { path, .. } => path.last().map(|i| i.name.clone()),
        _ => None,
    }
}

/// `[KEY]` as a property name: `KEY`.
fn computed(name: &str) -> Option<&str> {
    let inner = name.strip_prefix('[')?.strip_suffix(']')?;
    (!inner.starts_with("Symbol.")).then_some(inner)
}

/// The named type `n` (`boolean`, `any`).
fn named_type(n: &str, span: Span) -> ast::TypeExpr {
    ast::TypeExpr {
        kind: ast::TypeExprKind::Named {
            path: vec![ast::Ident {
                name: n.to_string(),
                span,
            }],
            args: vec![],
        },
        span,
    }
}

/// A type [`literal_type`] or [`named_type`] made, for comparing (`null` for `null`).
fn type_text(t: &ast::TypeExpr) -> &str {
    match &t.kind {
        ast::TypeExprKind::Named { path, .. } => path.last().map_or("", |i| i.name.as_str()),
        _ => "null",
    }
}

/// The type of a literal brand value, as written (`true` → `boolean`).
fn literal_type(e: &ast::Expr) -> Option<ast::TypeExpr> {
    let span = e.span;
    let named = |n: &str| named_type(n, span);
    Some(match &e.kind {
        E::Lit(ast::Lit::Bool(_)) => named("boolean"),
        E::Lit(ast::Lit::Str(_)) => named("string"),
        E::Lit(ast::Lit::Int { .. } | ast::Lit::Float { .. }) => named("number"),
        E::Lit(ast::Lit::Null) => ast::TypeExpr {
            kind: ast::TypeExprKind::Null,
            span,
        },
        _ => return None,
    })
}

impl Ctx<'_> {
    /// The key names of the fields `defineProperty` brands hide (not `enumerable: true`),
    /// resolved once.
    pub(crate) fn hidden_keys(&mut self) -> FxHashSet<String> {
        if let Some(h) = &self.objects.hidden {
            return h.clone();
        }
        let sites = self.objects.brands.sites.clone();
        let mut set = FxHashSet::default();
        for b in sites.iter().filter(|b| !b.enumerable) {
            if let Ok(k) = self.resolve_key(b.module, &computed_key(&b.key)) {
                set.insert(k);
            }
        }
        self.objects.hidden = Some(set.clone());
        set
    }

    /// The attributes (`enumerable`, `writable`, `configurable`) every `defineProperty` call
    /// with key name `key` gives (`own`, the attributes of a call the scan didn't record), or
    /// `None` if two of them differ: Velt keeps them per symbol.
    pub(crate) fn brand_attrs(
        &mut self,
        key: &str,
        own: (bool, bool, bool),
    ) -> Option<(bool, bool, bool)> {
        let sites = self.objects.brands.sites.clone();
        let mut found = None;
        for b in &sites {
            if self
                .resolve_key(b.module, &computed_key(&b.key))
                .ok()
                .as_deref()
                != Some(key)
            {
                continue;
            }
            let attrs = (b.enumerable, b.writable, b.configurable);
            match found {
                Some(f) if f != attrs => return None,
                _ => found = Some(attrs),
            }
        }
        Some(found.unwrap_or(own))
    }

    /// Where a property with key name `key` is written directly (`{ [KEY]: v }`,
    /// `o[KEY] = v`), if anywhere.
    pub(crate) fn direct_write(&mut self, key: &str) -> Option<Span> {
        let direct = self.objects.brands.direct.clone();
        direct.into_iter().find_map(|(m, k)| {
            let found = self.resolve_key(m, &computed_key(&k)).ok()?;
            (found == key).then_some(k.span)
        })
    }
}

/// `KEY` as the computed key `[KEY]` the parser writes (its span `KEY` alone,
/// `symbols::is_computed_key`).
fn computed_key(k: &ast::Ident) -> ast::Ident {
    ast::Ident {
        name: format!("[{}]", k.name),
        span: k.span,
    }
}
