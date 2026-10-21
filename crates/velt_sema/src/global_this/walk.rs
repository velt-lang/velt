//! The rewrite of one module's uses of `globalThis` (see the parent module): each property
//! read or write becomes the property's variable, a `const g = globalThis as T;` is dropped
//! (its uses stand for `globalThis`), and `Object.defineProperty(globalThis, KEY, d)` is
//! normalized to `Object.defineProperty(globalThis, "K", d)`.

use rustc_hash::FxHashMap;
use velt_common::Diagnostic;
use velt_syntax::ast::{self, ExprKind as E, ItemKind as I, StmtKind as S};
use velt_syntax::visit::{self, Visit};

use super::{slot_name, Site};
use crate::ast_walk::children_mut;
use crate::body::expr::NODE_GLOBALS;
use crate::SourceModule;

const GLOBAL_THIS: &str = "globalThis";

/// The names of `globalThis` in Node: itself, and `global`.
fn names_global(name: &str) -> bool {
    name == GLOBAL_THIS || name == "global"
}

/// Does `module` name `globalThis` in an expression?
pub(super) fn mentions(module: &ast::Module) -> bool {
    struct Finds(bool);
    impl Visit<'_> for Finds {
        fn expr(&mut self, e: &ast::Expr) {
            self.0 |= matches!(&e.kind, E::Ident(id) if names_global(&id.name));
        }
    }
    let mut f = Finds(false);
    visit::walk_module(module, &mut f);
    f.0
}

/// The module-level string constants of `module` (`const K = "k"`, or `"k" as const`),
/// including those the parser moved into a script's generated `main`.
pub(super) fn string_consts(module: &ast::Module) -> FxHashMap<String, String> {
    let mut out = FxHashMap::default();
    let mut add = |v: &ast::VarDecl| {
        if let (ast::VarKind::Const, ast::PatternKind::Ident(n), Some(init)) =
            (v.kind, &v.pattern.kind, &v.init)
        {
            if let Some(s) = string_lit(init) {
                out.insert(n.name.clone(), s);
            }
        }
    };
    for item in &module.items {
        match &item.kind {
            I::Var(v) => add(v),
            I::Function(f)
                if f.sig.name.name == "main" && f.sig.name.span.lo == f.sig.name.span.hi =>
            {
                for s in &f.body.stmts {
                    if let S::Var(v) = &s.kind {
                        add(v);
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// The string constants module `i` sees: its own, and those it imports by name.
pub(super) fn visible_consts(
    m: &SourceModule,
    modules: &[SourceModule],
    consts: &[FxHashMap<String, String>],
    i: usize,
) -> FxHashMap<String, String> {
    let mut out = consts[i].clone();
    for item in &m.ast.items {
        let I::Import(imp) = &item.kind else { continue };
        let Some((_, path)) = m.imports.iter().find(|(spec, _)| *spec == imp.from) else {
            continue;
        };
        let Some(from) = modules.iter().position(|o| o.path == *path) else {
            continue;
        };
        for n in &imp.names {
            if let Some(v) = consts[from].get(&n.name.name) {
                let local = n.alias.as_ref().unwrap_or(&n.name);
                out.insert(local.name.clone(), v.clone());
            }
        }
    }
    out
}

/// `"k"`, `` `k` `` or `"k" as const`.
fn string_lit(e: &ast::Expr) -> Option<String> {
    match &e.kind {
        E::Lit(ast::Lit::Str(s)) => Some(s.clone()),
        E::Template { quasis, exprs } if exprs.is_empty() => quasis.first().cloned(),
        E::Paren(x) | E::Cast { expr: x, .. } => string_lit(x),
        _ => None,
    }
}

/// `any` or `unknown`: an assertion that says nothing about the properties.
fn says_nothing(t: &ast::TypeExpr) -> bool {
    matches!(&t.kind, ast::TypeExprKind::Named { path, args }
        if args.is_empty() && path.len() == 1 && matches!(path[0].name.as_str(), "any" | "unknown"))
}

/// A name in scope: a constant standing for `globalThis` (with the type it asserts), or
/// another declaration (`None`), which hides an outer one of the same name.
type Binding = Option<Option<ast::TypeExpr>>;

pub(super) struct Walk<'a> {
    module: usize,
    consts: &'a FxHashMap<String, String>,
    scopes: Vec<FxHashMap<String, Binding>>,
    pub sites: Vec<Site>,
    pub diags: Vec<Diagnostic>,
}

impl<'a> Walk<'a> {
    pub(super) fn new(module: usize, consts: &'a FxHashMap<String, String>) -> Self {
        Walk {
            module,
            consts,
            scopes: vec![FxHashMap::default()],
            sites: vec![],
            diags: vec![],
        }
    }

    /// Rewrite every item of the module, its constants standing for `globalThis` first (a
    /// function may use one declared after it).
    pub(super) fn module_items(&mut self, module: &mut ast::Module) {
        // Other module-level names are the module's constants (string keys), unless they hide
        // `globalThis` or `global`.
        for item in &module.items {
            let mut names = vec![];
            match &item.kind {
                I::Var(v) if self.alias_of(v).is_some() => self.declare(v),
                I::Var(v) => pattern_names(&v.pattern, &mut names),
                I::Function(f) => names.push(f.sig.name.name.clone()),
                I::Import(imp) => names.extend(
                    (imp.names.iter()).map(|n| n.alias.as_ref().unwrap_or(&n.name).name.clone()),
                ),
                _ => {}
            }
            names.retain(|n| names_global(n));
            self.shadow(names);
        }
        module.items.retain(|item| match &item.kind {
            I::Var(v) => self.alias_of(v).is_none(),
            _ => true,
        });
        for item in &mut module.items {
            self.item(item);
        }
    }

    /// Record what `v` binds in the innermost scope.
    fn declare(&mut self, v: &ast::VarDecl) {
        let alias = self.alias_of(v);
        let mut names = vec![];
        pattern_names(&v.pattern, &mut names);
        let scope = self.scopes.last_mut().expect("ICE: no scope");
        for n in names {
            scope.insert(n, alias.clone());
        }
    }

    /// The type `v` asserts when it is `const g = globalThis as T;` (`Some(None)`: no type).
    fn alias_of(&self, v: &ast::VarDecl) -> Option<Option<ast::TypeExpr>> {
        if v.kind != ast::VarKind::Const || v.ty.is_some() {
            return None;
        }
        let ast::PatternKind::Ident(_) = &v.pattern.kind else {
            return None;
        };
        self.base(v.init.as_ref()?)
    }

    fn lookup(&self, name: &str) -> Option<&Binding> {
        self.scopes.iter().rev().find_map(|s| s.get(name))
    }

    /// The type asserted for `e` when it stands for `globalThis` (`Some(None)`: no type).
    fn base(&self, e: &ast::Expr) -> Option<Option<ast::TypeExpr>> {
        match &e.kind {
            E::Paren(x) | E::NonNull(x) => self.base(x),
            E::Cast { expr, ty } => {
                let inner = self.base(expr)?;
                Some(if says_nothing(ty) {
                    inner
                } else {
                    Some(ty.clone())
                })
            }
            E::Ident(id) => match self.lookup(&id.name) {
                Some(b) => b.clone(),
                None => names_global(&id.name).then_some(None),
            },
            _ => None,
        }
    }

    /// The property key `e` names: a string literal or constant.
    fn key(&self, e: &ast::Expr) -> Option<String> {
        if let E::Ident(id) = &e.kind {
            if self.lookup(&id.name).is_some() {
                return None;
            }
            return self.consts.get(&id.name).cloned();
        }
        string_lit(e)
    }

    fn with_scope(&mut self, f: impl FnOnce(&mut Self)) {
        self.scopes.push(FxHashMap::default());
        f(self);
        self.scopes.pop();
    }

    /// Hide outer constants named like `names` in the innermost scope.
    fn shadow(&mut self, names: Vec<String>) {
        let scope = self.scopes.last_mut().expect("ICE: no scope");
        for n in names {
            scope.insert(n, None);
        }
    }

    fn item(&mut self, item: &mut ast::Item) {
        match &mut item.kind {
            I::Function(f) => self.func(f),
            I::Struct(t) | I::Class(t) => {
                for e in t.fields.iter_mut().filter_map(|f| f.default.as_mut()) {
                    self.expr(e, false);
                }
                if let Some(c) = &mut t.constructor {
                    self.func(c);
                }
                for m in &mut t.methods {
                    self.func(&mut m.decl);
                }
            }
            I::Extend(e) => {
                for m in &mut e.methods {
                    self.func(&mut m.decl);
                }
            }
            I::Interface(i) => {
                for b in i.methods.iter_mut().filter_map(|m| m.body.as_mut()) {
                    self.block(b);
                }
            }
            I::Var(v) => {
                if let Some(e) = &mut v.init {
                    self.expr(e, false);
                }
            }
            I::Augment(a) => a.items.iter_mut().for_each(|i| self.item(i)),
            I::Namespace(n) => n.items.iter_mut().for_each(|i| self.item(i)),
            I::Import(_) | I::Enum(_) | I::TypeAlias(_) | I::ExternFn(_) => {}
        }
    }

    fn func(&mut self, f: &mut ast::FnDecl) {
        self.with_scope(|w| {
            w.shadow(params_names(&f.sig.params));
            for e in f.sig.params.iter_mut().filter_map(|p| p.default.as_mut()) {
                w.expr(e, false);
            }
            w.block(&mut f.body);
        });
    }

    fn block(&mut self, b: &mut ast::Block) {
        self.with_scope(|w| w.stmts(&mut b.stmts));
    }

    /// Statements of one scope: a constant standing for `globalThis` is dropped.
    fn stmts(&mut self, stmts: &mut [ast::Stmt]) {
        for s in stmts {
            if let S::Var(v) = &s.kind {
                if self.alias_of(v).is_some() {
                    let v = v.clone();
                    self.declare(&v);
                    s.kind = S::Empty;
                    continue;
                }
            }
            self.stmt(s);
        }
    }

    fn stmt(&mut self, s: &mut ast::Stmt) {
        match &mut s.kind {
            S::Var(v) => {
                if let Some(e) = &mut v.init {
                    self.expr(e, false);
                }
                let mut names = vec![];
                pattern_names(&v.pattern, &mut names);
                self.shadow(names);
            }
            S::Expr(e) => self.expr(e, true),
            S::Throw(e) | S::Return(Some(e)) => self.expr(e, false),
            S::If { cond, then, els } => {
                self.expr(cond, false);
                self.block(then);
                if let Some(e) = els {
                    self.with_scope(|w| w.stmt(e));
                }
            }
            S::While { cond, body } | S::DoWhile { body, cond } => {
                self.expr(cond, false);
                self.block(body);
            }
            S::For {
                init,
                cond,
                update,
                body,
            } => self.with_scope(|w| {
                if let Some(i) = init {
                    w.stmt(i);
                }
                if let Some(c) = cond {
                    w.expr(c, false);
                }
                if let Some(u) = update {
                    w.expr(u, true);
                }
                w.block(body);
            }),
            S::ForOf {
                pattern,
                iter,
                body,
                ..
            } => {
                self.expr(iter, false);
                let mut names = vec![];
                pattern_names(pattern, &mut names);
                self.with_scope(|w| {
                    w.shadow(names);
                    w.block(body);
                });
            }
            S::Block(b) => self.block(b),
            S::Labeled { body, .. } => self.stmt(body),
            S::Switch {
                discriminant,
                cases,
            } => {
                self.expr(discriminant, false);
                self.with_scope(|w| {
                    for c in cases {
                        if let Some(t) = &mut c.test {
                            w.expr(t, false);
                        }
                        w.stmts(&mut c.body);
                    }
                });
            }
            S::Try {
                body,
                catch,
                finally,
            } => {
                self.block(body);
                if let Some((p, c)) = catch {
                    let mut names = vec![];
                    if let Some(p) = p {
                        pattern_names(p, &mut names);
                    }
                    self.with_scope(|w| {
                        w.shadow(names);
                        w.block(c);
                    });
                }
                if let Some(f) = finally {
                    self.block(f);
                }
            }
            S::Item(item) => self.item(item),
            S::Return(None) | S::Break(_) | S::Continue(_) | S::Empty => {}
        }
    }

    /// Rewrite `e`; `stmt`: `e` is a statement of its own (its value is unused).
    fn expr(&mut self, e: &mut ast::Expr, stmt: bool) {
        if self.property(e, false) || self.define_property(e, stmt) {
            return;
        }
        match &mut e.kind {
            E::Assign { target, value, .. } => {
                if !self.property(target, true) {
                    self.expr(target, false);
                }
                self.expr(value, false);
                return;
            }
            E::Update { target, .. } => {
                if !self.property(target, true) {
                    self.expr(target, false);
                }
                return;
            }
            // A constant standing for `globalThis`, used as a value: `globalThis` itself,
            // which the checker reports.
            E::Ident(id)
                if id.name != GLOBAL_THIS && matches!(self.lookup(&id.name), Some(Some(_))) =>
            {
                let span = id.span;
                e.kind = E::Ident(ast::Ident {
                    name: GLOBAL_THIS.to_string(),
                    span,
                });
                return;
            }
            E::Arrow { params, body, .. } => {
                let mut names: Vec<String> = params.iter().map(|p| p.name.name.clone()).collect();
                for p in params.iter().filter_map(|p| p.pattern.as_ref()) {
                    pattern_names(p, &mut names);
                }
                self.with_scope(|w| {
                    w.shadow(names);
                    for d in params.iter_mut().filter_map(|p| p.default.as_mut()) {
                        w.expr(d, false);
                    }
                    match body {
                        ast::ArrowBody::Block(b) => w.block(b),
                        ast::ArrowBody::Expr(x) => w.expr(x, false),
                    }
                });
                return;
            }
            E::Function(d) => {
                self.func(d);
                return;
            }
            E::Object(props) => {
                for p in props.iter_mut() {
                    if let ast::ObjectProp::Method(d) = p {
                        self.func(d);
                    }
                }
            }
            _ => {}
        }
        children_mut(e, &mut |c| self.expr(c, false));
    }

    /// `e` as a property of `globalThis` (`written`: assigned): replaced by its variable, or by
    /// the Node global of that name. Whether it was one.
    fn property(&mut self, e: &mut ast::Expr, written: bool) -> bool {
        let (ty, key) = match &e.kind {
            E::Member { object, prop, .. } => match self.base(object) {
                Some(t) => (t, prop.name.clone()),
                None => return false,
            },
            E::Index { object, index, .. } => match (self.base(object), self.key(index)) {
                (Some(t), Some(k)) => (t, k),
                _ => return false,
            },
            _ => return false,
        };
        let span = e.span;
        if NODE_GLOBALS.iter().any(|(n, _)| *n == key) {
            e.kind = E::Ident(ast::Ident { name: key, span });
            return true;
        }
        self.sites.push(Site {
            module: self.module,
            key: key.clone(),
            ty,
            span,
            written,
        });
        e.kind = E::Ident(ast::Ident {
            name: slot_name(&key),
            span,
        });
        true
    }

    /// `Object.defineProperty(g, KEY, d)` with `g` standing for `globalThis`: normalized to
    /// `Object.defineProperty(globalThis, "K", d)`. Whether it was one.
    fn define_property(&mut self, e: &mut ast::Expr, stmt: bool) -> bool {
        let span = e.span;
        let E::Call { callee, args, .. } = &mut e.kind else {
            return false;
        };
        let E::Member { object, prop, .. } = &callee.kind else {
            return false;
        };
        let is_object = matches!(&object.kind, E::Ident(o) if o.name == "Object" && self.lookup("Object").is_none());
        if !is_object || prop.name != "defineProperty" || args.len() != 3 {
            return false;
        }
        let Some(ty) = self.base(&args[0]) else {
            return false;
        };
        let Some(key) = self.key(&args[1]) else {
            return false;
        };
        let site = Site {
            module: self.module,
            key: key.clone(),
            ty,
            span: args[0].span.to(args[1].span),
            written: false,
        };
        if !stmt {
            // Reported here; the checker sees the property.
            self.sites.push(site);
            e.kind = E::Ident(ast::Ident {
                name: slot_name(&key),
                span,
            });
            self.diags.push(
                Diagnostic::error(
                    "the result of `Object.defineProperty` on `globalThis` is not supported yet",
                    span,
                )
                .with_note("it is `globalThis`, which Velt has as a bag of typed properties, not as a value: call it as a statement of its own"),
            );
            return true;
        }
        self.sites.push(site);
        let o = args[0].span;
        args[0].kind = E::Ident(ast::Ident {
            name: GLOBAL_THIS.to_string(),
            span: o,
        });
        args[1].kind = E::Lit(ast::Lit::Str(key));
        self.expr(&mut args[2], false);
        true
    }
}

/// The names parameters bind.
fn params_names(params: &[ast::Param]) -> Vec<String> {
    let mut names: Vec<String> = params.iter().map(|p| p.name.name.clone()).collect();
    for p in params.iter().filter_map(|p| p.pattern.as_ref()) {
        pattern_names(p, &mut names);
    }
    names
}

/// The names pattern `p` binds.
fn pattern_names(p: &ast::Pattern, out: &mut Vec<String>) {
    match &p.kind {
        ast::PatternKind::Ident(id) => out.push(id.name.clone()),
        ast::PatternKind::Wildcard => {}
        ast::PatternKind::Object { fields, rest } => {
            fields.iter().for_each(|(_, p)| pattern_names(p, out));
            out.extend(rest.iter().map(|r| r.name.clone()));
        }
        ast::PatternKind::Array { elems, rest } => {
            elems.iter().for_each(|p| pattern_names(p, out));
            out.extend(rest.iter().map(|r| r.name.clone()));
        }
        ast::PatternKind::Default { pattern, .. } => pattern_names(pattern, out),
    }
}
