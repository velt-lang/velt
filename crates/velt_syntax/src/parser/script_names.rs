//! The names code refers to without declaring them (for `script`, and sema's check of the root
//! file's early uses): which top-level variables a function, class, constant or top-level
//! statement uses. Scopes are JavaScript's: a parameter is its function's, a `let`/`const`, a
//! class or a function declared in a block is that block's (all of it: a use above the
//! declaration in the block is the block's own, in its temporal dead zone), a `for` or `catch`
//! binding is its loop's or its `catch` clause's. Member names and object keys are not references.

use rustc_hash::FxHashSet;
use velt_common::Span;

use crate::ast::*;

/// A name that code reads without declaring it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FreeUse {
    pub name: String,
    /// The name as written.
    pub span: Span,
    /// In a function, arrow, method or nested declaration the code makes (it runs when called),
    /// not in the code that runs at once.
    pub deferred: bool,
}

/// The free names of `item`.
pub(super) fn free_names(item: &Item) -> FxHashSet<String> {
    let mut n = Names::default();
    match &item.kind {
        // A top-level variable's initializer runs at once; the rest is the item's own code.
        ItemKind::Var(v) => {
            let mut own = FxHashSet::default();
            pattern_names(&v.pattern, &mut own);
            n.scoped(own, |n| n.var(v));
        }
        _ => n.item(item),
    }
    let mut out = n.finish();
    // `typeof x` in any type the item writes refers to `x` too.
    out.extend(query_heads(item));
    out
}

/// The free names of the top-level statement `s`.
pub(super) fn stmt_free_names(s: &Stmt) -> FxHashSet<String> {
    let mut n = Names::default();
    n.stmts(std::slice::from_ref(s));
    n.finish()
}

/// The names top-level statement `s` reads without declaring them, in source order.
pub fn stmt_free_uses(s: &Stmt) -> Vec<FreeUse> {
    let mut n = Names::default();
    n.stmts(std::slice::from_ref(s));
    n.uses
}

/// The names expression `e` reads without declaring them, in source order.
pub fn expr_free_uses(e: &Expr) -> Vec<FreeUse> {
    let mut n = Names::default();
    n.expr(e);
    n.uses
}

/// The first names of the type queries (`typeof x.y`) written anywhere in `item`.
fn query_heads(item: &Item) -> Vec<String> {
    struct Heads(Vec<String>);
    impl<'a> crate::visit::Visit<'a> for Heads {
        fn ty(&mut self, t: &'a TypeExpr) {
            if let TypeExprKind::Query(path) = &t.kind {
                if let Some(h) = path.first() {
                    self.0.push(h.name.clone());
                }
            }
        }
    }
    let mut heads = Heads(vec![]);
    crate::visit::walk_item(item, &mut heads);
    heads.0
}

/// The names `p` binds.
pub(crate) fn pattern_names(p: &Pattern, out: &mut FxHashSet<String>) {
    match &p.kind {
        PatternKind::Ident(i) => {
            out.insert(i.name.clone());
        }
        PatternKind::Wildcard => {}
        PatternKind::Object { fields, rest } => {
            fields.iter().for_each(|(_, p)| pattern_names(p, out));
            out.extend(rest.iter().map(|r| r.name.clone()));
        }
        PatternKind::Array { elems, rest } => {
            elems.iter().for_each(|p| pattern_names(p, out));
            out.extend(rest.iter().map(|r| r.name.clone()));
        }
        PatternKind::Default { pattern, .. } => pattern_names(pattern, out),
    }
}

/// The names statement `s` declares in the scope it stands in.
fn declared(s: &Stmt, out: &mut FxHashSet<String>) {
    match &s.kind {
        StmtKind::Var(v) => pattern_names(&v.pattern, out),
        StmtKind::Item(item) => match &item.kind {
            ItemKind::Function(f) => {
                out.insert(f.sig.name.name.clone());
            }
            ItemKind::Class(d) | ItemKind::Struct(d) => {
                out.insert(d.name.name.clone());
            }
            ItemKind::Enum(e) => {
                out.insert(e.name.name.clone());
            }
            ItemKind::Var(v) => pattern_names(&v.pattern, out),
            _ => {}
        },
        _ => {}
    }
}

/// A parameter: its name, its pattern and its default.
type ParamParts<'a> = (&'a Ident, Option<&'a Pattern>, Option<&'a Expr>);

fn fn_params(params: &[Param]) -> Vec<ParamParts<'_>> {
    params
        .iter()
        .map(|p| (&p.name, p.pattern.as_ref(), p.default.as_ref()))
        .collect()
}

#[derive(Default)]
struct Names {
    uses: Vec<FreeUse>,
    /// The names each enclosing scope declares, innermost last.
    scopes: Vec<FxHashSet<String>>,
    /// The functions entered.
    depth: u32,
}

impl Names {
    fn finish(self) -> FxHashSet<String> {
        self.uses.into_iter().map(|u| u.name).collect()
    }

    fn use_name(&mut self, name: &str, span: Span) {
        if !self.scopes.iter().any(|s| s.contains(name)) {
            self.uses.push(FreeUse {
                name: name.to_string(),
                span,
                deferred: self.depth > 0,
            });
        }
    }

    fn scoped(&mut self, names: FxHashSet<String>, f: impl FnOnce(&mut Self)) {
        self.scopes.push(names);
        f(self);
        self.scopes.pop();
    }

    /// Statements that make one scope: a block, a `switch`'s cases, a top-level statement.
    fn stmts<'s>(&mut self, stmts: impl IntoIterator<Item = &'s Stmt> + Clone) {
        let mut names = FxHashSet::default();
        stmts
            .clone()
            .into_iter()
            .for_each(|s| declared(s, &mut names));
        self.scoped(names, |n| stmts.into_iter().for_each(|s| n.stmt(s)));
    }

    fn block(&mut self, b: &Block) {
        self.stmts(&b.stmts);
    }

    /// A function, method or arrow: its own name (a named function expression's), its
    /// parameters and its body, which run when it is called.
    fn function(
        &mut self,
        own: Option<&Ident>,
        params: &[ParamParts],
        body: impl FnOnce(&mut Self),
    ) {
        let mut names: FxHashSet<String> = own.iter().map(|i| i.name.clone()).collect();
        for (name, pattern, _) in params {
            names.insert(name.name.clone());
            pattern.iter().for_each(|p| pattern_names(p, &mut names));
        }
        self.depth += 1;
        self.scoped(names, |n| {
            for (_, pattern, default) in params {
                pattern.iter().for_each(|p| n.pattern(p));
                default.iter().for_each(|e| n.expr(e));
            }
            body(n);
        });
        self.depth -= 1;
    }

    fn fn_decl(&mut self, f: &FnDecl) {
        self.function(None, &fn_params(&f.sig.params), |n| n.block(&f.body));
    }

    fn var(&mut self, v: &VarDecl) {
        self.pattern(&v.pattern);
        if let Some(e) = &v.init {
            self.expr(e);
        }
    }

    /// A declaration: its name is declared where it stands ([`declared`]); its code runs later.
    fn item(&mut self, item: &Item) {
        match &item.kind {
            ItemKind::Function(f) => self.fn_decl(f),
            ItemKind::Class(d) | ItemKind::Struct(d) => {
                self.depth += 1;
                for f in &d.fields {
                    if let Some(e) = &f.default {
                        self.expr(e);
                    }
                }
                self.depth -= 1;
                d.methods.iter().for_each(|m| self.fn_decl(&m.decl));
                if let Some(c) = &d.constructor {
                    self.fn_decl(c);
                }
            }
            ItemKind::Extend(d) => d.methods.iter().for_each(|m| self.fn_decl(&m.decl)),
            ItemKind::Var(v) => self.var(v),
            ItemKind::Import(_)
            | ItemKind::Augment(_)
            | ItemKind::Namespace(_)
            | ItemKind::Interface(_)
            | ItemKind::Enum(_)
            | ItemKind::TypeAlias(_)
            | ItemKind::ExternFn(_) => {}
        }
    }

    fn stmt(&mut self, s: &Stmt) {
        match &s.kind {
            StmtKind::Var(v) => self.var(v),
            StmtKind::Expr(e) | StmtKind::Throw(e) => self.expr(e),
            StmtKind::Return(e) => e.iter().for_each(|e| self.expr(e)),
            StmtKind::If { cond, then, els } => {
                self.expr(cond);
                self.block(then);
                els.iter().for_each(|s| self.stmt(s));
            }
            StmtKind::While { cond, body } | StmtKind::DoWhile { body, cond } => {
                self.expr(cond);
                self.block(body);
            }
            StmtKind::For {
                init,
                cond,
                update,
                body,
            } => {
                let mut names = FxHashSet::default();
                init.iter().for_each(|s| declared(s, &mut names));
                self.scoped(names, |n| {
                    init.iter().for_each(|s| n.stmt(s));
                    cond.iter().chain(update).for_each(|e| n.expr(e));
                    n.block(body);
                });
            }
            StmtKind::ForOf {
                pattern,
                iter,
                body,
                ..
            } => {
                let mut names = FxHashSet::default();
                pattern_names(pattern, &mut names);
                // The iterated expression is in the loop's scope too (its bindings' dead zone).
                self.scoped(names, |n| {
                    n.pattern(pattern);
                    n.expr(iter);
                    n.block(body);
                });
            }
            StmtKind::Block(b) => self.block(b),
            StmtKind::Labeled { body, .. } => self.stmt(body),
            StmtKind::Switch {
                discriminant,
                cases,
            } => {
                self.expr(discriminant);
                let bodies = cases.iter().flat_map(|c| &c.body);
                let mut names = FxHashSet::default();
                bodies.clone().for_each(|s| declared(s, &mut names));
                self.scoped(names, |n| {
                    for c in cases {
                        c.test.iter().for_each(|e| n.expr(e));
                        c.body.iter().for_each(|s| n.stmt(s));
                    }
                });
            }
            StmtKind::Try {
                body,
                catch,
                finally,
            } => {
                self.block(body);
                if let Some((p, b)) = catch {
                    let mut names = FxHashSet::default();
                    p.iter().for_each(|p| pattern_names(p, &mut names));
                    self.scoped(names, |n| {
                        p.iter().for_each(|p| n.pattern(p));
                        n.block(b);
                    });
                }
                finally.iter().for_each(|b| self.block(b));
            }
            StmtKind::Item(item) => {
                self.depth += 1;
                self.item(item);
                self.depth -= 1;
            }
            StmtKind::Break(_) | StmtKind::Continue(_) | StmtKind::Empty => {}
        }
    }

    /// What a binding pattern reads: its computed keys and its defaults.
    fn pattern(&mut self, p: &Pattern) {
        match &p.kind {
            PatternKind::Ident(_) | PatternKind::Wildcard => {}
            PatternKind::Object { fields, .. } => {
                for (key, p) in fields {
                    self.computed_key(key);
                    self.pattern(p);
                }
            }
            PatternKind::Array { elems, .. } => elems.iter().for_each(|p| self.pattern(p)),
            PatternKind::Default { pattern, value } => {
                self.pattern(pattern);
                self.expr(value);
            }
        }
    }

    /// `{ [k]: v }` reads `k` (also as an assignment target or a pattern).
    fn computed_key(&mut self, key: &Ident) {
        if let Some(k) = crate::computed_key_name(key) {
            self.use_name(k, key.span);
        }
    }

    fn expr(&mut self, e: &Expr) {
        match &e.kind {
            ExprKind::Lit(_) | ExprKind::This | ExprKind::Super => {}
            ExprKind::Ident(id) => self.use_name(&id.name, id.span),
            ExprKind::Template { exprs, .. } | ExprKind::Array(exprs) => {
                exprs.iter().for_each(|e| self.expr(e))
            }
            ExprKind::Unary { expr, .. }
            | ExprKind::Update { target: expr, .. }
            | ExprKind::Spread(expr)
            | ExprKind::Await(expr)
            | ExprKind::Cast { expr, .. }
            | ExprKind::InstanceOf { expr, .. }
            | ExprKind::Paren(expr)
            | ExprKind::NonNull(expr)
            | ExprKind::Member { object: expr, .. } => self.expr(expr),
            ExprKind::Binary { lhs, rhs, .. }
            | ExprKind::Assign {
                target: lhs,
                value: rhs,
                ..
            }
            | ExprKind::Index {
                object: lhs,
                index: rhs,
                ..
            } => {
                self.expr(lhs);
                self.expr(rhs);
            }
            ExprKind::Cond { cond, then, els } => {
                self.expr(cond);
                self.expr(then);
                self.expr(els);
            }
            ExprKind::Call { callee, args, .. } => {
                self.expr(callee);
                args.iter().for_each(|e| self.expr(e));
            }
            ExprKind::New { class, args } => {
                // `new C()` reads `C`.
                if let TypeExprKind::Named { path, .. } = &class.kind {
                    if let [head] = path.as_slice() {
                        self.use_name(&head.name, head.span);
                    }
                }
                args.iter().for_each(|e| self.expr(e));
            }
            ExprKind::Arrow { params, body, .. } => {
                let params: Vec<ParamParts> = params
                    .iter()
                    .map(|p| (&p.name, p.pattern.as_ref(), p.default.as_ref()))
                    .collect();
                self.function(None, &params, |n| match body {
                    ArrowBody::Expr(e) => n.expr(e),
                    ArrowBody::Block(b) => n.block(b),
                });
            }
            ExprKind::Function(f) => {
                let own = Some(&f.sig.name).filter(|n| !n.name.is_empty());
                self.function(own, &fn_params(&f.sig.params), |n| n.block(&f.body));
            }
            ExprKind::Object(props) | ExprKind::StructLit { props, .. } => {
                for p in props {
                    match p {
                        ObjectProp::KeyValue(key, e) => {
                            self.computed_key(key);
                            self.expr(e);
                        }
                        ObjectProp::Spread(e) => self.expr(e),
                        ObjectProp::Shorthand(id) => self.use_name(&id.name, id.span),
                        ObjectProp::Method(f) => self.fn_decl(f),
                    }
                }
            }
            ExprKind::Yield { arg, .. } => arg.iter().for_each(|e| self.expr(e)),
            ExprKind::Jsx(el) => self.jsx(el),
        }
    }

    fn jsx(&mut self, el: &JsxElement) {
        for a in &el.attrs {
            match a {
                JsxAttr::Named { value, .. } => match value {
                    Some(JsxAttrValue::Expr { expr, .. }) => self.expr(expr),
                    Some(JsxAttrValue::Element(el)) => self.jsx(el),
                    Some(JsxAttrValue::Str { .. }) | None => {}
                },
                JsxAttr::Spread { expr, .. } => self.expr(expr),
            }
        }
        for c in &el.children {
            match c {
                JsxChild::Expr { expr, .. } => expr.iter().for_each(|e| self.expr(e)),
                JsxChild::Spread { expr, .. } => self.expr(expr),
                JsxChild::Element(el) => self.jsx(el),
                JsxChild::Text { .. } => {}
            }
        }
    }
}
