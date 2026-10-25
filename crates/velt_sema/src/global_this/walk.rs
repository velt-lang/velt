//! The rewrite of one module's uses of `globalThis` (see the parent module): each property
//! read or write becomes the property's variable, a `const g = globalThis as T;` is dropped
//! (its uses stand for `globalThis`), and `Object.defineProperty(globalThis, KEY, d)` is
//! normalized to `Object.defineProperty(globalThis, "K", d)`.

use rustc_hash::FxHashMap;
use velt_common::Diagnostic;
use velt_syntax::ast::{self, ExprKind as E, ItemKind as I, StmtKind as S};

use super::keys::{
    listed_keys, names_global, params_names, pattern_names, says_nothing, string_lit, GLOBAL_THIS,
};
use super::{slot_name, Site};
use crate::ast_walk::children_mut;
use crate::body::expr::NODE_GLOBALS;

/// A name in scope: a constant standing for `globalThis` (with the type it asserts), or
/// another declaration (`None`), which hides an outer one of the same name.
type Binding = Option<Option<ast::TypeExpr>>;

pub(super) struct Walk<'a> {
    module: usize,
    consts: &'a FxHashMap<String, String>,
    /// The module's type aliases, for the keys an asserted type lists.
    aliases: &'a FxHashMap<String, ast::TypeExpr>,
    scopes: Vec<FxHashMap<String, Binding>>,
    /// The string constants each scope declares (`const KEY = "k";`, also a script's
    /// module-level ones, which the parser moved into its `main`): keys there.
    const_scopes: Vec<FxHashMap<String, String>>,
    pub sites: Vec<Site>,
    pub diags: Vec<Diagnostic>,
}

impl<'a> Walk<'a> {
    pub(super) fn new(
        module: usize,
        consts: &'a FxHashMap<String, String>,
        aliases: &'a FxHashMap<String, ast::TypeExpr>,
    ) -> Self {
        Walk {
            module,
            consts,
            aliases,
            scopes: vec![FxHashMap::default()],
            const_scopes: vec![FxHashMap::default()],
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

    /// The property key `e` names: a string literal or constant (the innermost declaration of
    /// the name decides).
    fn key(&self, e: &ast::Expr) -> Option<String> {
        if let E::Ident(id) = &e.kind {
            for (names, consts) in self.scopes.iter().zip(&self.const_scopes).rev() {
                if let Some(k) = consts.get(&id.name) {
                    return Some(k.clone());
                }
                if names.contains_key(&id.name) {
                    return None;
                }
            }
            return self.consts.get(&id.name).cloned();
        }
        string_lit(e)
    }

    fn with_scope(&mut self, f: impl FnOnce(&mut Self)) {
        self.scopes.push(FxHashMap::default());
        self.const_scopes.push(FxHashMap::default());
        f(self);
        self.scopes.pop();
        self.const_scopes.pop();
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
                if let (ast::VarKind::Const, ast::PatternKind::Ident(n), Some(k)) = (
                    v.kind,
                    &v.pattern.kind,
                    v.init.as_ref().and_then(string_lit),
                ) {
                    let scope = self.const_scopes.last_mut().expect("ICE: no scope");
                    scope.insert(n.name.clone(), k);
                }
            }
            S::Expr(e) => match self.define_listed(e).or_else(|| self.assign_listed(e)) {
                Some(chain) => s.kind = chain,
                None => self.expr(e, true),
            },
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
                (Some(Some(t)), None) if !written => {
                    let Some((k, keys)) = self.listed(&t, index) else {
                        return false;
                    };
                    let span = e.span;
                    let mut chain = synth(E::Lit(ast::Lit::Undefined), span);
                    for key in keys.into_iter().rev() {
                        let mut read = synth(
                            E::Member {
                                object: Box::new(synth(
                                    E::Paren(Box::new(synth(
                                        E::Cast {
                                            expr: Box::new(ident(GLOBAL_THIS, span)),
                                            ty: t.clone(),
                                        },
                                        span,
                                    ))),
                                    span,
                                )),
                                prop: ast::Ident {
                                    name: key.clone(),
                                    span,
                                },
                                optional: false,
                            },
                            span,
                        );
                        self.property(&mut read, false);
                        chain = synth(
                            E::Cond {
                                cond: Box::new(is_key(&k, &key, span)),
                                then: Box::new(read),
                                els: Box::new(chain),
                            },
                            span,
                        );
                    }
                    *e = synth(E::Paren(Box::new(chain)), span);
                    return true;
                }
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
            defined: false,
        });
        e.kind = E::Ident(ast::Ident {
            name: slot_name(&key),
            span,
        });
        true
    }

    /// The variable `index` names and the keys the asserted type `t` lists, when `index` is a
    /// variable (not a string constant) and `t` lists a fixed set of keys.
    fn listed(&self, t: &ast::TypeExpr, index: &ast::Expr) -> Option<(ast::Ident, Vec<String>)> {
        let E::Ident(k) = &index.kind else {
            return None;
        };
        if matches!(self.lookup(&k.name), Some(Some(_))) {
            return None;
        }
        Some((k.clone(), listed_keys(t, self.aliases)?))
    }

    /// `Object.defineProperty(g, k, d);` with `g` standing for `globalThis` as a type listing a
    /// fixed set of keys and `k` a variable: `if (k === "a") Object.defineProperty(g, "a", d);
    /// else if ...`, each branch rewritten as a definition of its key.
    fn define_listed(&mut self, e: &ast::Expr) -> Option<S> {
        let E::Call { callee, args, .. } = &e.kind else {
            return None;
        };
        let E::Member { object, prop, .. } = &callee.kind else {
            return None;
        };
        let is_object = matches!(&object.kind, E::Ident(o) if o.name == "Object" && self.lookup("Object").is_none());
        if !is_object
            || prop.name != "defineProperty"
            || args.len() != 3
            || self.key(&args[1]).is_some()
        {
            return None;
        }
        let Some(Some(t)) = self.base(&args[0]) else {
            return None;
        };
        let (k, keys) = self.listed(&t, &args[1])?;
        let span = e.span;
        let mut els: Option<Box<ast::Stmt>> = None;
        for key in keys.into_iter().rev() {
            let mut call = e.clone();
            if let E::Call { args, .. } = &mut call.kind {
                args[1] = synth(E::Lit(ast::Lit::Str(key.clone())), args[1].span);
            }
            self.define_property(&mut call, true);
            let then = ast::Block {
                stmts: vec![ast::Stmt {
                    kind: S::Expr(call),
                    span,
                }],
                span,
            };
            els = Some(Box::new(ast::Stmt {
                kind: S::If {
                    cond: is_key(&k, &key, span),
                    then,
                    els,
                },
                span,
            }));
        }
        els.map(|s| s.kind)
    }

    /// `g[k] = v;` (or `op=`) with `g` standing for `globalThis` as a type listing a fixed set
    /// of keys and `k` a variable: `if (k === "a") g.a = v; else if ...`, each branch rewritten
    /// as an assignment of its key.
    fn assign_listed(&mut self, e: &ast::Expr) -> Option<S> {
        let E::Assign { op, target, value } = &e.kind else {
            return None;
        };
        let E::Index { object, index, .. } = &target.kind else {
            return None;
        };
        if self.key(index).is_some() {
            return None;
        }
        let Some(Some(t)) = self.base(object) else {
            return None;
        };
        let (k, keys) = self.listed(&t, index)?;
        let span = e.span;
        let mut els: Option<Box<ast::Stmt>> = None;
        for key in keys.into_iter().rev() {
            let member = synth(
                E::Member {
                    object: Box::new(synth(
                        E::Paren(Box::new(synth(
                            E::Cast {
                                expr: Box::new(ident(GLOBAL_THIS, span)),
                                ty: t.clone(),
                            },
                            span,
                        ))),
                        span,
                    )),
                    prop: ast::Ident {
                        name: key.clone(),
                        span: target.span,
                    },
                    optional: false,
                },
                target.span,
            );
            let mut assign = synth(
                E::Assign {
                    op: *op,
                    target: Box::new(member),
                    value: value.clone(),
                },
                span,
            );
            self.expr(&mut assign, true);
            let then = ast::Block {
                stmts: vec![ast::Stmt {
                    kind: S::Expr(assign),
                    span,
                }],
                span,
            };
            els = Some(Box::new(ast::Stmt {
                kind: S::If {
                    cond: is_key(&k, &key, span),
                    then,
                    els,
                },
                span,
            }));
        }
        els.map(|s| s.kind)
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
            defined: stmt,
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

fn synth(kind: E, span: velt_common::Span) -> ast::Expr {
    ast::Expr {
        id: ast::NodeId(u32::MAX),
        kind,
        span,
    }
}

fn ident(name: &str, span: velt_common::Span) -> ast::Expr {
    synth(
        E::Ident(ast::Ident {
            name: name.to_string(),
            span,
        }),
        span,
    )
}

/// `(k as string) === "key"`: the cast keeps `k` from narrowing, so a test in a branch of
/// another (a read in the value of an assignment) is not always false.
fn is_key(k: &ast::Ident, key: &str, span: velt_common::Span) -> ast::Expr {
    let string = ast::TypeExpr {
        kind: ast::TypeExprKind::Named {
            path: vec![ast::Ident {
                name: "string".into(),
                span: k.span,
            }],
            args: vec![],
        },
        span: k.span,
    };
    let k = synth(
        E::Cast {
            expr: Box::new(synth(E::Ident(k.clone()), k.span)),
            ty: string,
        },
        k.span,
    );
    synth(
        E::Binary {
            op: ast::BinaryOp::StrictEq,
            lhs: Box::new(k),
            rhs: Box::new(synth(E::Lit(ast::Lit::Str(key.to_string())), span)),
        },
        span,
    )
}
