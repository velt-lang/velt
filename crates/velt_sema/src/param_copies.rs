//! Arrow functions that assign one of their parameters (`(child, parent, anchor) => { if (…)
//! anchor = null; … }`, sigx's `nodeOps.insert`), which JavaScript allows: the parameter is the
//! function's own variable, and the caller's argument doesn't change. A closure borrows its
//! arguments (its parameters have the fixed modes of a function type), so it can't assign one
//! (`ownership::finish::reassigned_fixed_params`). Before checking, such a parameter is renamed
//! to a name no source can spell, and the body starts with a variable of the parameter's name:
//!
//! ```text
//! (child, parent, anchor) => { B }   →   (child, parent, <param anchor>) => { let anchor = <param anchor>; B }
//! ```
//!
//! The variable is the function's own value (a share of the argument, as any `let` of a
//! borrowed value is), which the body may assign. An expression body becomes a block that
//! returns it.
//!
//! A parameter is renamed when the arrow's body assigns it (`=`, a compound assignment, `++`,
//! `--`), directly or in a function nested in it, and nothing in the body declares that name
//! again (a variable, a parameter, a function or class, a `catch` binding), and no other
//! parameter's default reads it. Destructured parameters and named functions are left alone.

use rustc_hash::FxHashSet;
use velt_common::Span;
use velt_syntax::ast::{self, ExprKind as E, ItemKind as I, StmtKind as S};
use velt_syntax::visit::{self, Visit};

use crate::ast_walk::children_mut;
use crate::SourceModule;

/// The prefix of a renamed parameter (module docs).
pub(crate) const PREFIX: &str = "<param ";

/// The program with its arrows' assigned parameters copied (`None`: no arrow assigns one).
pub(crate) fn rewrite(modules: &[SourceModule]) -> Option<Vec<SourceModule>> {
    // Std is not scanned: none of its arrows assigns a parameter, which was an error before.
    let found = crate::par::map(modules, crate::par::module_weight(modules), |m| {
        !m.is_std && found_in(&m.ast)
    });
    if !found.contains(&true) {
        return None;
    }
    let work: Vec<(&SourceModule, bool)> = modules.iter().zip(found).collect();
    let done = crate::par::map(&work, crate::par::module_weight(modules), |&(m, found)| {
        let mut items = m.ast.items.clone();
        if found {
            let mut rw = Rw;
            for i in &mut items {
                rw.item(i);
            }
        }
        SourceModule {
            path: m.path.clone(),
            is_std: m.is_std,
            is_dependency: m.is_dependency,
            file: m.file,
            ast: ast::Module {
                items,
                span: m.ast.span,
                jsx_import_source: m.ast.jsx_import_source.clone(),
                fn_names: m.ast.fn_names.clone(),
            },
            imports: m.imports.clone(),
            jsx_runtime: m.jsx_runtime.clone(),
        }
    });
    Some(done)
}

fn found_in(m: &ast::Module) -> bool {
    let mut f = Finds(false);
    visit::walk_module(m, &mut f);
    f.0
}

/// Does some arrow assign one of its parameters that may be renamed?
struct Finds(bool);

impl<'a> Visit<'a> for Finds {
    fn expr(&mut self, e: &'a ast::Expr) {
        if !self.0 && matches!(e.kind, E::Arrow { .. }) && !renamed(e).is_empty() {
            self.0 = true;
        }
    }
}

/// The parameters of arrow `e` to rename (module docs), by position.
fn renamed(e: &ast::Expr) -> Vec<usize> {
    let E::Arrow { params, body, .. } = &e.kind else {
        return vec![];
    };
    let simple: Vec<(usize, &str)> = (params.iter().enumerate())
        .filter(|(_, p)| p.pattern.is_none() && !p.name.name.starts_with(['#', '<']))
        .map(|(i, p)| (i, p.name.name.as_str()))
        .collect();
    if simple.is_empty() {
        return vec![];
    }
    let mut scan = Scan::default();
    match body {
        ast::ArrowBody::Expr(x) => visit::walk_expr(x, &mut scan),
        ast::ArrowBody::Block(b) => visit::walk_block(b, &mut scan),
    }
    let mut out = vec![];
    for (i, name) in simple {
        if !scan.assigned.contains(name) || scan.declared.contains(name) {
            continue;
        }
        // Another parameter's default reads it.
        let read = params.iter().filter_map(|p| p.default.as_ref()).any(|d| {
            let mut r = Reads(name, false);
            visit::walk_expr(d, &mut r);
            r.1
        });
        if !read {
            out.push(i);
        }
    }
    out
}

/// The names a body assigns, and those it declares.
#[derive(Default)]
struct Scan<'a> {
    assigned: FxHashSet<&'a str>,
    declared: FxHashSet<&'a str>,
}

/// The names pattern `p` binds.
fn pattern_names<'a>(p: &'a ast::Pattern, out: &mut FxHashSet<&'a str>) {
    match &p.kind {
        ast::PatternKind::Ident(i) => {
            out.insert(i.name.as_str());
        }
        ast::PatternKind::Wildcard => {}
        ast::PatternKind::Object { fields, rest } => {
            fields.iter().for_each(|(_, f)| pattern_names(f, out));
            out.extend(rest.iter().map(|r| r.name.as_str()));
        }
        ast::PatternKind::Array { elems, rest } => {
            elems.iter().for_each(|e| pattern_names(e, out));
            out.extend(rest.iter().map(|r| r.name.as_str()));
        }
        ast::PatternKind::Default { pattern, .. } => pattern_names(pattern, out),
    }
}

impl<'a> Visit<'a> for Scan<'a> {
    fn var_decl(&mut self, v: &'a ast::VarDecl) {
        pattern_names(&v.pattern, &mut self.declared);
    }

    fn stmt(&mut self, s: &'a ast::Stmt) {
        match &s.kind {
            S::ForOf { pattern, .. } => pattern_names(pattern, &mut self.declared),
            S::Try {
                catch: Some((Some(p), _)),
                ..
            } => pattern_names(p, &mut self.declared),
            _ => {}
        }
    }

    fn item(&mut self, item: &'a ast::Item) {
        match &item.kind {
            I::Function(f) => {
                self.declared.insert(f.sig.name.name.as_str());
            }
            I::Class(t) | I::Struct(t) => {
                self.declared.insert(t.name.name.as_str());
            }
            _ => {}
        }
    }

    fn function(&mut self, sig: &'a ast::FnSig, _body: &'a ast::Block) {
        for p in &sig.params {
            self.declared.insert(p.name.name.as_str());
            if let Some(pat) = &p.pattern {
                pattern_names(pat, &mut self.declared);
            }
        }
    }

    fn expr(&mut self, e: &'a ast::Expr) {
        match &e.kind {
            E::Assign { target, .. } | E::Update { target, .. } => {
                if let E::Ident(i) = &strip(target).kind {
                    self.assigned.insert(i.name.as_str());
                }
            }
            E::Arrow { params, .. } => {
                for p in params {
                    self.declared.insert(p.name.name.as_str());
                    if let Some(pat) = &p.pattern {
                        pattern_names(pat, &mut self.declared);
                    }
                }
            }
            E::Function(d) => {
                self.declared.insert(d.sig.name.name.as_str());
                for p in &d.sig.params {
                    self.declared.insert(p.name.name.as_str());
                }
            }
            _ => {}
        }
    }
}

/// Does an expression read name `.0`?
struct Reads<'n>(&'n str, bool);

impl<'a> Visit<'a> for Reads<'_> {
    fn expr(&mut self, e: &'a ast::Expr) {
        if matches!(&e.kind, E::Ident(i) if i.name == self.0) {
            self.1 = true;
        }
    }
}

fn strip(e: &ast::Expr) -> &ast::Expr {
    match &e.kind {
        E::Paren(x) | E::NonNull(x) => strip(x),
        _ => e,
    }
}

/// The rewrite of every arrow in a module (module docs).
struct Rw;

impl Rw {
    fn item(&mut self, item: &mut ast::Item) {
        match &mut item.kind {
            I::Function(f) => self.func(f),
            I::Struct(t) | I::Class(t) => {
                for e in t.fields.iter_mut().filter_map(|f| f.default.as_mut()) {
                    self.expr(e);
                }
                if let Some(c) = &mut t.constructor {
                    self.func(c);
                }
                for m in &mut t.methods {
                    self.func(&mut m.decl);
                }
            }
            I::Extend(e) => e.methods.iter_mut().for_each(|m| self.func(&mut m.decl)),
            I::Interface(i) => {
                for b in i.methods.iter_mut().filter_map(|m| m.body.as_mut()) {
                    self.block(b);
                }
            }
            I::Var(v) => {
                if let Some(e) = &mut v.init {
                    self.expr(e);
                }
            }
            I::Augment(a) => a.items.iter_mut().for_each(|i| self.item(i)),
            I::Namespace(n) => n.items.iter_mut().for_each(|i| self.item(i)),
            I::Import(_) | I::Enum(_) | I::TypeAlias(_) | I::ExternFn(_) => {}
        }
    }

    fn func(&mut self, f: &mut ast::FnDecl) {
        for e in f.sig.params.iter_mut().filter_map(|p| p.default.as_mut()) {
            self.expr(e);
        }
        self.block(&mut f.body);
    }

    fn block(&mut self, b: &mut ast::Block) {
        for s in &mut b.stmts {
            self.stmt(s);
        }
    }

    fn opt_expr(&mut self, e: Option<&mut ast::Expr>) {
        if let Some(e) = e {
            self.expr(e);
        }
    }

    fn stmt(&mut self, s: &mut ast::Stmt) {
        match &mut s.kind {
            S::Var(v) => self.opt_expr(v.init.as_mut()),
            S::Expr(e) | S::Throw(e) | S::Return(Some(e)) => self.expr(e),
            S::If { cond, then, els } => {
                self.expr(cond);
                self.block(then);
                if let Some(e) = els {
                    self.stmt(e);
                }
            }
            S::While { cond, body } | S::DoWhile { body, cond } => {
                self.expr(cond);
                self.block(body);
            }
            S::For {
                init,
                cond,
                update,
                body,
            } => {
                if let Some(i) = init {
                    self.stmt(i);
                }
                self.opt_expr(cond.as_mut());
                self.opt_expr(update.as_mut());
                self.block(body);
            }
            S::ForOf { iter, body, .. } => {
                self.expr(iter);
                self.block(body);
            }
            S::Block(b) | S::Unsafe(b) => self.block(b),
            S::Labeled { body, .. } => self.stmt(body),
            S::Switch {
                discriminant,
                cases,
            } => {
                self.expr(discriminant);
                for c in cases {
                    self.opt_expr(c.test.as_mut());
                    for s in &mut c.body {
                        self.stmt(s);
                    }
                }
            }
            S::Try {
                body,
                catch,
                finally,
            } => {
                self.block(body);
                if let Some((_, c)) = catch {
                    self.block(c);
                }
                if let Some(f) = finally {
                    self.block(f);
                }
            }
            S::Item(item) => self.item(item),
            S::Return(None) | S::Break(_) | S::Continue(_) | S::Empty => {}
        }
    }

    fn expr(&mut self, e: &mut ast::Expr) {
        let renames = renamed(e);
        match &mut e.kind {
            // `m.with((v) => { v = … })` on a `Mutex` (`rw.write(…)` on a `RwLock`) gets the
            // locked value by reference, so assigning `v` updates it (`body::expr::sync`): its
            // arrow is left as it is.
            E::Call { callee, args, .. } if lock_callback(callee, args).is_some() => {
                self.expr(callee);
                if let Some(E::Arrow { params, body, .. }) =
                    lock_callback(callee, args).map(|i| &mut strip_mut(&mut args[i]).kind)
                {
                    self.arrow_inside(params, body);
                }
                return;
            }
            E::Arrow { params, body, .. } => {
                if !renames.is_empty() {
                    copy_params(params, body, &renames, e.span);
                }
                self.arrow_inside(params, body);
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
        children_mut(e, &mut |c| self.expr(c));
    }

    /// The body and the parameters' defaults of an arrow.
    fn arrow_inside(&mut self, params: &mut [ast::ArrowParam], body: &mut ast::ArrowBody) {
        match body {
            ast::ArrowBody::Block(b) => self.block(b),
            ast::ArrowBody::Expr(x) => self.expr(x),
        }
        for d in params.iter_mut().filter_map(|p| p.default.as_mut()) {
            self.expr(d);
        }
    }
}

/// The index of the arrow argument of `recv.with(arrow)` or `recv.write(arrow)` (module docs).
fn lock_callback(callee: &ast::Expr, args: &[ast::Expr]) -> Option<usize> {
    let E::Member { prop, .. } = &callee.kind else {
        return None;
    };
    let ([arg], "with" | "write") = (args, prop.name.as_str()) else {
        return None;
    };
    matches!(strip(arg).kind, E::Arrow { .. }).then_some(0)
}

fn strip_mut(e: &mut ast::Expr) -> &mut ast::Expr {
    match e.kind {
        E::Paren(_) | E::NonNull(_) => match &mut e.kind {
            E::Paren(x) | E::NonNull(x) => strip_mut(x),
            _ => unreachable!("ICE: matched above"),
        },
        _ => e,
    }
}

/// Rename parameters `renames` of an arrow and start its body with their copies.
fn copy_params(
    params: &mut [ast::ArrowParam],
    body: &mut ast::ArrowBody,
    renames: &[usize],
    span: Span,
) {
    let mut lets = vec![];
    for &i in renames {
        let p = &mut params[i];
        let name = p.name.clone();
        p.name.name = format!("{PREFIX}{}>", name.name);
        let init = ast::Expr {
            id: ast::NodeId(u32::MAX),
            kind: E::Ident(p.name.clone()),
            span: name.span,
        };
        lets.push(ast::Stmt {
            kind: S::Var(ast::VarDecl {
                kind: ast::VarKind::Let,
                pattern: ast::Pattern {
                    id: ast::NodeId(u32::MAX),
                    kind: ast::PatternKind::Ident(name.clone()),
                    span: name.span,
                },
                ty: None,
                init: Some(init),
                span: name.span,
                c_const: false,
                c_global: None,
            }),
            span: name.span,
        });
    }
    match body {
        ast::ArrowBody::Block(b) => {
            lets.append(&mut b.stmts);
            b.stmts = lets;
        }
        ast::ArrowBody::Expr(x) => {
            let value = std::mem::replace(
                &mut **x,
                ast::Expr {
                    id: ast::NodeId(u32::MAX),
                    kind: E::Lit(ast::Lit::Undefined),
                    span,
                },
            );
            let at = value.span;
            lets.push(ast::Stmt {
                kind: S::Return(Some(value)),
                span: at,
            });
            *body = ast::ArrowBody::Block(ast::Block {
                stmts: lets,
                span: at,
            });
        }
    }
}
