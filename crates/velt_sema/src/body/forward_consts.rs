//! Variables that closures written before them use (#1142, docs/reference/functions.md "A
//! closure using the constant it initializes"): `const effectFn = () => { … runJob(); }; const
//! runJob = () => { … };`, `queueMicrotask(() => stop()); … const stop = () => { … };`, and the
//! same with `let`.
//!
//! JavaScript's `const` and `let` are in scope in their whole block; a closure created before
//! the declaration reads the variable when it runs, which is fine once the declaration ran (the
//! temporal dead zone is checked at run time). Such a variable is a late `const` (`late_consts`)
//! whose initializer is "running" from the first statement of its block with a closure using
//! it to its own declaration: it is declared (with its annotated type, or the type a trial
//! check of the statements in between and its initializer gives, once per declaration) and left
//! unassigned at that first statement, and assigned at its declaration. Closures created in
//! between capture the variable (a shared cell). When something in between may run code (a
//! call, a getter), a ready flag is captured too and a read before the assignment throws
//! JavaScript's `ReferenceError`, as for a closure in the constant's own initializer; when
//! everything in between only creates values, no closure can run before the assignment, and
//! there is no flag.
//!
//! A use counts when it is in a closure (an arrow, a function expression, an object-literal
//! method) of the statement and no scope between the use and the block binds the name. A
//! destructuring declaration or a `let` without an initializer used so is an error (not
//! supported yet).

use rustc_hash::FxHashMap;

use velt_common::{Diagnostic, Span};
use velt_syntax::ast::{self, ExprKind as E, ItemKind as I, StmtKind as S};

use super::{FnCx, LocalKind, Want};
use crate::hir::TyId;

/// A variable of a block used by a closure written before its declaration.
pub(super) struct Forward {
    /// The first statement with such a closure.
    pub first: usize,
    /// The declaration's statement.
    pub decl: usize,
    /// The name as declared.
    name: String,
    /// The binding's span.
    at: Span,
    /// May code run between `first` and the assignment (a ready flag is needed)?
    flag: bool,
}

/// How a block declares a name.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    /// `const x = e` / `let x = e`.
    Simple,
    /// A name a destructuring pattern binds, or `let x;`.
    Other,
}

/// The variables of block `stmts` that a closure in a statement before their declaration uses
/// (module docs). One walk of the statements.
pub(super) fn find(stmts: &[ast::Stmt]) -> Vec<Forward> {
    let mut later: FxHashMap<&str, (usize, Kind, Span)> = FxHashMap::default();
    for (i, s) in stmts.iter().enumerate().skip(1) {
        let S::Var(v) = &s.kind else { continue };
        if v.pattern.id == ast::NodeId(u32::MAX) {
            continue; // a nested function's closure constant (`nested_closures` hoists it)
        }
        match (&v.pattern.kind, &v.init) {
            (ast::PatternKind::Ident(n), Some(_)) => {
                later.insert(n.name.as_str(), (i, Kind::Simple, n.span));
            }
            _ => {
                let mut names = vec![];
                pattern_names(&v.pattern, &mut names);
                for n in names {
                    later.insert(n.name.as_str(), (i, Kind::Other, n.span));
                }
            }
        }
    }
    let Some(last) = later.values().map(|d| d.0).max() else {
        return vec![];
    };
    let mut w = Walker {
        later: &later,
        cur: 0,
        shadow: FxHashMap::default(),
        closure: 0,
        hits: FxHashMap::default(),
    };
    for (j, s) in stmts[..last].iter().enumerate() {
        w.cur = j;
        w.root(s);
    }
    let mut out: Vec<Forward> = w
        .hits
        .into_iter()
        .map(|(name, first)| {
            let (decl, kind, at) = later[name];
            let flag = kind == Kind::Other || !inert_between(stmts, first, decl);
            Forward {
                first,
                decl,
                name: name.to_string(),
                at,
                flag,
            }
        })
        .collect();
    // A later declaration first: its trial checks the earlier ones it may use (`a` before `b`
    // in `const f = () => a() + b(); const b = () => a() + 1; const a = () => 1;`).
    out.sort_by_key(|f| (f.first, std::cmp::Reverse(f.decl), f.at.lo));
    out
}

/// May nothing from statement `first` to the assignment of the declaration at `decl` run code
/// (only closures, literals and names are evaluated there)?
fn inert_between(stmts: &[ast::Stmt], first: usize, decl: usize) -> bool {
    stmts[first..=decl].iter().all(|s| match &s.kind {
        S::Var(v) => {
            matches!(v.pattern.kind, ast::PatternKind::Ident(_))
                && v.init.as_ref().is_none_or(inert)
        }
        S::Item(item) => matches!(item.kind, I::Function(_)),
        S::Empty => true,
        _ => false,
    })
}

/// Does evaluating `e` only create a value (no call, no getter, no iteration)?
fn inert(e: &ast::Expr) -> bool {
    match &e.kind {
        E::Arrow { .. } | E::Function(_) | E::Lit(_) | E::Ident(_) => true,
        E::Paren(x) | E::Cast { expr: x, .. } | E::NonNull(x) => inert(x),
        E::Array(xs) => xs
            .iter()
            .all(|x| !matches!(x.kind, E::Spread(_)) && inert(x)),
        E::Object(props) | E::StructLit { props, .. } => props.iter().all(|p| match p {
            ast::ObjectProp::KeyValue(k, v) => {
                let computed = k.name.starts_with('[');
                (!computed || velt_syntax::computed_key_name(k).is_some()) && inert(v)
            }
            ast::ObjectProp::Shorthand(_) | ast::ObjectProp::Method(_) => true,
            ast::ObjectProp::Spread(_) => false,
        }),
        _ => false,
    }
}

/// The names pattern `p` binds.
fn pattern_names<'a>(p: &'a ast::Pattern, out: &mut Vec<&'a ast::Ident>) {
    use ast::PatternKind as P;
    match &p.kind {
        P::Ident(id) => out.push(id),
        P::Object { fields, rest } => {
            fields.iter().for_each(|(_, q)| pattern_names(q, out));
            out.extend(rest.iter());
        }
        P::Array { elems, rest } => {
            elems.iter().for_each(|q| pattern_names(q, out));
            out.extend(rest.iter());
        }
        P::Default { pattern, .. } => pattern_names(pattern, out),
        P::Wildcard => {}
    }
}

/// The names the statements of a block declare (variables, nested functions and classes).
fn block_names(stmts: &[ast::Stmt]) -> Vec<&ast::Ident> {
    let mut out = vec![];
    for s in stmts {
        match &s.kind {
            S::Var(v) => pattern_names(&v.pattern, &mut out),
            S::Item(item) => match &item.kind {
                I::Function(f) => out.push(&f.sig.name),
                I::Class(t) | I::Struct(t) => out.push(&t.name),
                _ => {}
            },
            _ => {}
        }
    }
    out
}

/// The walk of [`find`]: the names in scope inside the statement (`shadow`, counted), the depth
/// of closures, and the first statement using each later name from a closure.
struct Walker<'a> {
    later: &'a FxHashMap<&'a str, (usize, Kind, Span)>,
    cur: usize,
    shadow: FxHashMap<String, u32>,
    closure: u32,
    hits: FxHashMap<&'a str, usize>,
}

impl<'a> Walker<'a> {
    fn bind(&mut self, names: &[&ast::Ident]) {
        for n in names {
            *self.shadow.entry(n.name.clone()).or_default() += 1;
        }
    }

    fn unbind(&mut self, names: &[&ast::Ident]) {
        for n in names {
            if let Some(c) = self.shadow.get_mut(&n.name) {
                *c -= 1;
            }
        }
    }

    fn name(&mut self, name: &str) {
        if self.closure == 0 || self.shadow.get(name).is_some_and(|c| *c > 0) {
            return;
        }
        if let Some((key, (decl, _, _))) = self.later.get_key_value(name) {
            if *decl > self.cur {
                self.hits.entry(key).or_insert(self.cur);
            }
        }
    }

    /// A statement of the block itself: its own declarations are the block's.
    fn root(&mut self, s: &ast::Stmt) {
        match &s.kind {
            S::Var(v) => self.var(v),
            // A class's methods cannot use the enclosing function's variables; a nested
            // function is `nested_closures`'.
            S::Item(_) => {}
            _ => self.stmt(s),
        }
    }

    fn var(&mut self, v: &ast::VarDecl) {
        self.pattern_defaults(&v.pattern);
        if let Some(e) = &v.init {
            self.expr(e);
        }
    }

    fn block(&mut self, stmts: &[ast::Stmt]) {
        let names = block_names(stmts);
        self.bind(&names);
        for s in stmts {
            self.stmt(s);
        }
        self.unbind(&names);
    }

    fn stmt(&mut self, s: &ast::Stmt) {
        match &s.kind {
            S::Var(v) => self.var(v),
            S::Expr(e) | S::Throw(e) | S::Return(Some(e)) => self.expr(e),
            S::If { cond, then, els } => {
                self.expr(cond);
                self.block(&then.stmts);
                if let Some(e) = els {
                    self.stmt(e);
                }
            }
            S::While { cond, body } | S::DoWhile { body, cond } => {
                self.expr(cond);
                self.block(&body.stmts);
            }
            S::For {
                init,
                cond,
                update,
                body,
            } => {
                let mut names = vec![];
                if let Some(S::Var(v)) = init.as_ref().map(|i| &i.kind) {
                    pattern_names(&v.pattern, &mut names);
                }
                self.bind(&names);
                if let Some(i) = init {
                    self.stmt(i);
                }
                cond.iter().chain(update).for_each(|e| self.expr(e));
                self.block(&body.stmts);
                self.unbind(&names);
            }
            S::ForOf {
                pattern,
                iter,
                body,
                ..
            } => {
                self.expr(iter);
                let mut names = vec![];
                pattern_names(pattern, &mut names);
                self.bind(&names);
                self.pattern_defaults(pattern);
                self.block(&body.stmts);
                self.unbind(&names);
            }
            S::Block(b) => self.block(&b.stmts),
            S::Labeled { body, .. } => self.stmt(body),
            S::Switch {
                discriminant,
                cases,
            } => {
                self.expr(discriminant);
                let all: Vec<&ast::Stmt> = cases.iter().flat_map(|c| &c.body).collect();
                let mut names = vec![];
                for s in &all {
                    names.extend(block_names(std::slice::from_ref(*s)));
                }
                self.bind(&names);
                for c in cases {
                    c.test.iter().for_each(|e| self.expr(e));
                    c.body.iter().for_each(|s| self.stmt(s));
                }
                self.unbind(&names);
            }
            S::Try {
                body,
                catch,
                finally,
            } => {
                self.block(&body.stmts);
                if let Some((p, b)) = catch {
                    let mut names = vec![];
                    if let Some(p) = p {
                        pattern_names(p, &mut names);
                    }
                    self.bind(&names);
                    self.block(&b.stmts);
                    self.unbind(&names);
                }
                if let Some(b) = finally {
                    self.block(&b.stmts);
                }
            }
            S::Item(_) | S::Return(None) | S::Break(_) | S::Continue(_) | S::Empty => {}
        }
    }

    fn pattern_defaults(&mut self, p: &ast::Pattern) {
        use ast::PatternKind as P;
        match &p.kind {
            P::Object { fields, .. } => fields.iter().for_each(|(_, q)| self.pattern_defaults(q)),
            P::Array { elems, .. } => elems.iter().for_each(|q| self.pattern_defaults(q)),
            P::Default { pattern, value } => {
                self.pattern_defaults(pattern);
                self.expr(value);
            }
            P::Ident(_) | P::Wildcard => {}
        }
    }

    fn func(&mut self, d: &ast::FnDecl) {
        let mut names: Vec<&ast::Ident> = vec![];
        if !d.sig.name.name.is_empty() {
            names.push(&d.sig.name);
        }
        for p in &d.sig.params {
            match &p.pattern {
                Some(q) => pattern_names(q, &mut names),
                None => names.push(&p.name),
            }
        }
        self.closure += 1;
        self.bind(&names);
        for p in &d.sig.params {
            if let Some(q) = &p.pattern {
                self.pattern_defaults(q);
            }
            if let Some(e) = &p.default {
                self.expr(e);
            }
        }
        self.block(&d.body.stmts);
        self.unbind(&names);
        self.closure -= 1;
    }

    fn expr(&mut self, e: &ast::Expr) {
        match &e.kind {
            E::Ident(id) => self.name(&id.name),
            E::Arrow { params, body, .. } => {
                let mut names: Vec<&ast::Ident> = vec![];
                for p in params {
                    match &p.pattern {
                        Some(q) => pattern_names(q, &mut names),
                        None => names.push(&p.name),
                    }
                }
                self.closure += 1;
                self.bind(&names);
                for p in params {
                    if let Some(q) = &p.pattern {
                        self.pattern_defaults(q);
                    }
                    if let Some(d) = &p.default {
                        self.expr(d);
                    }
                }
                match body {
                    ast::ArrowBody::Block(b) => self.block(&b.stmts),
                    ast::ArrowBody::Expr(x) => self.expr(x),
                }
                self.unbind(&names);
                self.closure -= 1;
            }
            E::Function(d) => self.func(d),
            E::Object(props) | E::StructLit { props, .. } => {
                for p in props {
                    match p {
                        ast::ObjectProp::Shorthand(id) => self.name(&id.name),
                        ast::ObjectProp::Method(d) => self.func(d),
                        ast::ObjectProp::KeyValue(_, v) | ast::ObjectProp::Spread(v) => {
                            self.expr(v)
                        }
                    }
                }
            }
            _ => crate::ast_walk::children(e, &mut |c| self.expr(c)),
        }
    }
}

impl FnCx<'_, '_> {
    /// Before statement `i` of `stmts`: declare the variables `forward` (from [`find`]) says
    /// statement `i` is the first to use (module docs).
    pub(super) fn forward_declare(
        &mut self,
        stmts: &[ast::Stmt],
        forward: &[Forward],
        i: usize,
        out: &mut Vec<crate::hir::Stmt>,
    ) {
        for f in forward.iter().filter(|f| f.first == i) {
            let S::Var(v) = &stmts[f.decl].kind else {
                continue;
            };
            let (ast::PatternKind::Ident(name), Some(init)) = (&v.pattern.kind, &v.init) else {
                self.forward_unsupported(f, v);
                continue;
            };
            let ty = match &v.ty {
                Some(t) => self.resolve(t),
                None => self.forward_trial_ty(&stmts[i..f.decl], name, init),
            };
            let kind = match v.kind {
                ast::VarKind::Let => LocalKind::Let,
                _ => LocalKind::Const,
            };
            let span = stmts[i].span;
            let (local, flag) = self.declare_late(name, ty, (span, kind), f.flag, out);
            self.f.late.forward.insert(name.span, (local, flag));
        }
    }

    /// A destructuring declaration or a `let` without an initializer used by a closure written
    /// before it: not supported yet.
    fn forward_unsupported(&mut self, f: &Forward, v: &ast::VarDecl) {
        let what = match v.pattern.kind {
            ast::PatternKind::Ident(_) => "a `let` without an initializer",
            _ => "a destructuring declaration",
        };
        let name = &f.name;
        self.cx.error(
            Diagnostic::error(
                format!("`{name}` is used by a closure written before its declaration, which is not supported yet for {what}"),
                f.at,
            )
            .with_note("TypeScript allows this (the variable is in scope in its whole block); Velt supports it for a declaration of a single name with a value")
            .with_note(format!("declare it as `const {name} = …` (or `let {name} = …`), or move the closure after the declaration")),
        );
    }

    /// The type of the initializer `init` of `name`, checked as a trial (rolled back) after the
    /// statements `between` it and its first use, with `name` of the error type; computed once
    /// per declaration (a trial inside another trial reuses it).
    fn forward_trial_ty(
        &mut self,
        between: &[ast::Stmt],
        name: &ast::Ident,
        init: &ast::Expr,
    ) -> TyId {
        if let Some(&t) = self.forward_tys.get(&name.span) {
            return t;
        }
        let mark = super::recheck::Mark::here(self.cx);
        let frames = self.trial_frames();
        let err = self.cx.ty.error;
        self.push_scope();
        self.declare_local(name, err, LocalKind::Const);
        let mut scratch = vec![];
        for s in between {
            self.stmt(s, &mut scratch);
        }
        let h = self.expr(init, None, Want::Move);
        let incomplete = self.incomplete_ty(h.ty, 0);
        mark.rollback(self.cx);
        self.restore_trial_frames(frames);
        if incomplete {
            self.cx.error(
                Diagnostic::error(
                    format!(
                        "type annotation needed for `{}`: a closure written before it uses it",
                        name.name
                    ),
                    name.span,
                )
                .with_note(format!("write its type, e.g. `const {}: T = …`", name.name)),
            );
        } else {
            self.forward_tys.insert(name.span, h.ty);
        }
        h.ty
    }

    /// `const name = init;` (or `let`) declared at its first use (`forward_declare`): its
    /// initializer, then the assignments. False when it is no such variable.
    pub(super) fn forward_const_decl(
        &mut self,
        v: &ast::VarDecl,
        name: &ast::Ident,
        span: Span,
        out: &mut Vec<crate::hir::Stmt>,
    ) -> bool {
        let Some((local, flag)) = self.f.late.forward.remove(&name.span) else {
            return false;
        };
        let Some(e) = v.init.as_ref() else {
            return false;
        };
        let ty = self.local_ty(local);
        let h = self.expr_coerce(e, ty, Want::Move);
        self.f.late.pending.remove(&local);
        self.assign_late(local, flag, ty, h, name.span, span, out);
        true
    }
}
