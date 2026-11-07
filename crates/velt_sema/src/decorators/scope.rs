//! What a name means where a decorator or one of its constants is written, before any item of that
//! name: decorators are resolved like TypeScript's, in the scope around the declaration, so a
//! local of an enclosing function hides a module-level decorator or constant, and a local in a
//! block the name isn't in (an arrow's parameter, a sibling block) hides nothing.
//!
//! The first decorator of a module asks for one walk of the module, which records each scope it
//! passes (a block, a function's parameters, a `for`'s binding, …) as the regions of the source
//! it covers and the names it binds. A name's answer is then a lookup: the innermost region that
//! holds it, by a binary search of the regions in source order, and the names of the regions
//! around that one. So a module's decorators cost one walk of it and a lookup each, whatever the
//! module and its scopes hold. A module without decorators is never walked.

use rustc_hash::FxHashMap;
use velt_common::{FileId, Span};
use velt_syntax::ast;

use crate::ctx::Ctx;

/// The innermost declaration of a name that hides module level.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Shadow {
    /// A parameter or a variable, or the binding of a `for` or a `catch`.
    Local,
    /// A nested item (`Ctx::lookup_item_at` finds those).
    Item,
}

/// What hides module level for the name `id`, written in module `m`.
pub(super) fn shadow_of(cx: &Ctx, m: usize, id: &ast::Ident) -> Option<Shadow> {
    let module = &cx.modules[m];
    if id.span.file != module.file {
        return None;
    }
    let mut cache = cx.decorator_scopes.borrow_mut();
    let scopes = cache
        .entry(m)
        .or_insert_with(|| Scopes::of(&module.ast.items, module.file));
    scopes.shadow(id)
}

/// The name `path` starts with in module `m`, when that is an `import type`: `T`, or for a member
/// of a namespace imported so, `N.x`. A nested item of that name hides the import.
pub(super) fn type_only_name(cx: &Ctx, m: usize, path: &[ast::Ident]) -> Option<String> {
    let head = path.first()?;
    let type_only = &cx.scopes[m].type_only;
    if type_only.is_empty() || shadow_of(cx, m, head).is_some() {
        return None;
    }
    if type_only.contains(&head.name) {
        return Some(head.name.clone());
    }
    // `N.x` is one key, whatever `N`'s own parts are.
    let names: Vec<&str> = path.iter().map(|p| p.name.as_str()).collect();
    (1..=names.len()).rev().find_map(|n| {
        let key = names[..n].join(".");
        type_only.contains(&key).then_some(key)
    })
}

/// No region: the module's own level.
const NONE: u32 = u32::MAX;

/// A part of the source where the names of scope `scope` are bound.
#[derive(Clone, Copy, Debug)]
struct Region {
    lo: u32,
    hi: u32,
    scope: u32,
    /// The innermost region around this one, or [`NONE`].
    parent: u32,
}

impl Region {
    fn holds(&self, lo: u32, hi: u32) -> bool {
        self.lo <= lo && hi <= self.hi
    }
}

/// A module's scopes below module level, by position; see the module docs.
pub(crate) struct Scopes {
    /// In source order, a region before those inside it (and before a later one of the same
    /// span, which the walk found inside it).
    regions: Vec<Region>,
    /// Each name bound in some scope, numbered.
    names: FxHashMap<String, u32>,
    /// What each scope binds: (scope, name) to what the name is there.
    bound: FxHashMap<(u32, u32), Shadow>,
}

impl Scopes {
    fn of(items: &[ast::Item], file: FileId) -> Scopes {
        let mut w = Walk::new(file);
        for item in items.iter().filter(|i| i.span.file == file) {
            w.item(item);
        }
        crate::check_work::add_decorator_scope_steps(w.steps);
        Scopes::from(w)
    }

    /// The regions walk `w` found, each told the one around it.
    fn from(w: Walk) -> Scopes {
        // Sorted by start, a longer region first, then in the walk's order: a region the walk
        // found inside another of the same span (a function's body inside its parameters'
        // scope) comes after it.
        let mut regions = w.regions;
        regions.sort_unstable_by_key(|(r, seq)| (r.lo, std::cmp::Reverse(r.hi), *seq));
        let mut out: Vec<Region> = Vec::with_capacity(regions.len());
        let mut open: Vec<u32> = vec![];
        for (mut r, _) in regions {
            while let Some(&top) = open.last() {
                if out[top as usize].holds(r.lo, r.hi) {
                    break;
                }
                open.pop();
            }
            r.parent = open.last().copied().unwrap_or(NONE);
            open.push(out.len() as u32);
            out.push(r);
        }
        Scopes {
            regions: out,
            names: w.names,
            bound: w.bound,
        }
    }

    /// What hides module level for `id` (see [`shadow_of`]).
    fn shadow(&self, id: &ast::Ident) -> Option<Shadow> {
        let name = *self.names.get(id.name.as_str())?;
        let (lo, hi) = (id.span.lo, id.span.hi);
        // The last region starting at or before the name holds it, or one around it does: a
        // region around the name that started before that one also holds that one.
        let starting = self.regions.partition_point(|r| r.lo <= lo);
        let mut at = starting.checked_sub(1)? as u32;
        let mut steps = 1;
        while at != NONE && !self.regions[at as usize].holds(lo, hi) {
            at = self.regions[at as usize].parent;
            steps += 1;
        }
        let mut found = None;
        while at != NONE {
            let r = self.regions[at as usize];
            if let Some(&s) = self.bound.get(&(r.scope, name)) {
                found = Some(s);
                break;
            }
            at = r.parent;
            steps += 1;
        }
        crate::check_work::add_decorator_scope_steps(steps);
        found
    }
}

struct Walk {
    file: FileId,
    /// Each region with its place in the walk's order.
    regions: Vec<(Region, u32)>,
    scopes: u32,
    names: FxHashMap<String, u32>,
    bound: FxHashMap<(u32, u32), Shadow>,
    /// Nodes looked at, for `check_work`.
    steps: u64,
}

impl Walk {
    fn new(file: FileId) -> Walk {
        Walk {
            file,
            regions: vec![],
            scopes: 0,
            names: FxHashMap::default(),
            bound: FxHashMap::default(),
            steps: 0,
        }
    }

    fn scope(&mut self) -> u32 {
        self.scopes += 1;
        self.scopes - 1
    }

    /// Scope `scope` covers `span`.
    fn region(&mut self, span: Span, scope: u32) {
        if span.file == self.file && span.lo <= span.hi {
            let seq = self.regions.len() as u32;
            let region = Region {
                lo: span.lo,
                hi: span.hi,
                scope,
                parent: NONE,
            };
            self.regions.push((region, seq));
        }
    }

    fn bind(&mut self, scope: u32, name: &str, what: Shadow) {
        let next = self.names.len() as u32;
        let n = match self.names.get(name) {
            Some(&n) => n,
            None => {
                self.names.insert(name.to_string(), next);
                next
            }
        };
        let entry = self.bound.entry((scope, n)).or_insert(what);
        if what == Shadow::Item {
            *entry = Shadow::Item;
        }
    }

    fn item(&mut self, item: &ast::Item) {
        use ast::ItemKind as I;
        self.steps += 1;
        match &item.kind {
            I::Function(f) => self.function(f),
            I::Var(v) => self.opt_expr(v.init.as_ref()),
            I::Struct(t) | I::Class(t) => {
                t.methods.iter().for_each(|m| self.function(&m.decl));
                t.constructor.iter().for_each(|c| self.function(c));
                t.fields
                    .iter()
                    .for_each(|f| self.opt_expr(f.default.as_ref()));
            }
            I::Extend(e) => e.methods.iter().for_each(|m| self.function(&m.decl)),
            I::Interface(i) => {
                for m in &i.methods {
                    if let Some(body) = &m.body {
                        self.params(&m.sig.params, body);
                    }
                }
            }
            _ => {}
        }
    }

    fn function(&mut self, f: &ast::FnDecl) {
        self.params(&f.sig.params, &f.body);
    }

    /// A function's parameters and body: its parameters are in scope there and in their
    /// defaults, and not in its own decorators or its parameters' (TypeScript resolves those
    /// outside the function).
    fn params(&mut self, params: &[ast::Param], body: &ast::Block) {
        let scope = self.scope();
        for p in params {
            match &p.pattern {
                Some(q) => self.bind_pattern(scope, q),
                None => self.bind(scope, &p.name.name, Shadow::Local),
            }
        }
        for p in params {
            if let Some(d) = &p.default {
                self.region(d.span, scope);
                self.expr(d);
            }
            if let Some(q) = &p.pattern {
                self.pattern_exprs(q, Some(scope));
            }
        }
        self.region(body.span, scope);
        self.block(body);
    }

    fn bind_pattern(&mut self, scope: u32, p: &ast::Pattern) {
        use ast::PatternKind as P;
        self.steps += 1;
        match &p.kind {
            P::Ident(id) => self.bind(scope, &id.name, Shadow::Local),
            P::Object { fields, rest } => {
                fields
                    .iter()
                    .for_each(|(_, sub)| self.bind_pattern(scope, sub));
                rest.iter()
                    .for_each(|r| self.bind(scope, &r.name, Shadow::Local));
            }
            P::Array { elems, rest } => {
                elems.iter().for_each(|sub| self.bind_pattern(scope, sub));
                rest.iter()
                    .for_each(|r| self.bind(scope, &r.name, Shadow::Local));
            }
            P::Default { pattern, .. } => self.bind_pattern(scope, pattern),
            P::Wildcard => {}
        }
    }

    /// The defaults inside pattern `p`; with `scope`, each is a region of it (a parameter's or
    /// a `catch`'s pattern, whose names its defaults see).
    fn pattern_exprs(&mut self, p: &ast::Pattern, scope: Option<u32>) {
        use ast::PatternKind as P;
        self.steps += 1;
        match &p.kind {
            P::Object { fields, .. } => fields
                .iter()
                .for_each(|(_, sub)| self.pattern_exprs(sub, scope)),
            P::Array { elems, .. } => elems.iter().for_each(|sub| self.pattern_exprs(sub, scope)),
            P::Default { pattern, value } => {
                self.pattern_exprs(pattern, scope);
                if let Some(s) = scope {
                    self.region(value.span, s);
                }
                self.expr(value);
            }
            P::Ident(_) | P::Wildcard => {}
        }
    }

    /// Block `b`: what it declares anywhere in it is in scope all over it.
    fn block(&mut self, b: &ast::Block) {
        let scope = self.scope();
        self.region(b.span, scope);
        for s in &b.stmts {
            match &s.kind {
                ast::StmtKind::Var(v) => self.bind_pattern(scope, &v.pattern),
                ast::StmtKind::Item(item) => {
                    if let Some(name) = item_name(item) {
                        self.bind(scope, name, Shadow::Item);
                    }
                }
                _ => {}
            }
        }
        for s in &b.stmts {
            self.stmt(s);
        }
    }

    fn stmt(&mut self, s: &ast::Stmt) {
        use ast::StmtKind as S;
        self.steps += 1;
        match &s.kind {
            S::Item(item) => self.item(item),
            S::Var(v) => {
                self.pattern_exprs(&v.pattern, None);
                self.opt_expr(v.init.as_ref());
            }
            S::Expr(e) | S::Throw(e) | S::Return(Some(e)) => self.expr(e),
            S::If { cond, then, els } => {
                self.expr(cond);
                self.block(then);
                els.iter().for_each(|e| self.stmt(e));
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
                let scope = self.scope();
                self.region(s.span, scope);
                if let Some(i) = init {
                    if let S::Var(v) = &i.kind {
                        self.bind_pattern(scope, &v.pattern);
                    }
                    self.stmt(i);
                }
                self.opt_expr(cond.as_ref());
                self.opt_expr(update.as_ref());
                self.block(body);
            }
            S::ForOf {
                pattern,
                iter,
                body,
                ..
            } => {
                let scope = self.scope();
                self.region(s.span, scope);
                self.bind_pattern(scope, pattern);
                self.pattern_exprs(pattern, None);
                self.expr(iter);
                self.block(body);
            }
            S::Block(b) | S::Unsafe(b) => self.block(b),
            S::Switch {
                discriminant,
                cases,
            } => {
                // One scope for all of a `switch`'s cases.
                let scope = self.scope();
                self.region(s.span, scope);
                self.expr(discriminant);
                for c in cases {
                    self.opt_expr(c.test.as_ref());
                }
                for st in cases.iter().flat_map(|c| &c.body) {
                    if let S::Var(v) = &st.kind {
                        self.bind_pattern(scope, &v.pattern);
                    }
                }
                for st in cases.iter().flat_map(|c| &c.body) {
                    self.stmt(st);
                }
            }
            S::Labeled { body, .. } => self.stmt(body),
            S::Try {
                body,
                catch,
                finally,
            } => {
                self.block(body);
                if let Some((pat, b)) = catch {
                    let scope = self.scope();
                    if let Some(p) = pat {
                        self.bind_pattern(scope, p);
                        self.pattern_exprs(p, Some(scope));
                    }
                    self.region(b.span, scope);
                    self.block(b);
                }
                finally.iter().for_each(|b| self.block(b));
            }
            S::Return(None) | S::Break(_) | S::Continue(_) | S::Empty => {}
        }
    }

    fn opt_expr(&mut self, e: Option<&ast::Expr>) {
        e.iter().for_each(|e| self.expr(e));
    }

    fn expr(&mut self, e: &ast::Expr) {
        use ast::ExprKind as E;
        self.steps += 1;
        match &e.kind {
            E::Arrow { params, body, .. } => {
                let scope = self.scope();
                self.region(e.span, scope);
                for p in params {
                    match &p.pattern {
                        Some(q) => self.bind_pattern(scope, q),
                        None => self.bind(scope, &p.name.name, Shadow::Local),
                    }
                }
                for p in params {
                    self.opt_expr(p.default.as_ref());
                    p.pattern.iter().for_each(|q| self.pattern_exprs(q, None));
                }
                match body {
                    ast::ArrowBody::Expr(x) => self.expr(x),
                    ast::ArrowBody::Block(b) => self.block(b),
                }
            }
            E::Function(d) => {
                let scope = self.scope();
                self.region(e.span, scope);
                self.bind(scope, &d.sig.name.name, Shadow::Local);
                self.function(d);
            }
            E::Object(props) | E::StructLit { props, .. } => {
                crate::ast_walk::children(e, &mut |c| self.expr(c));
                for p in props {
                    if let ast::ObjectProp::Method(d) = p {
                        self.function(d);
                    }
                }
            }
            _ => crate::ast_walk::children(e, &mut |c| self.expr(c)),
        }
    }
}

/// The name item `item` declares in a block.
fn item_name(item: &ast::Item) -> Option<&str> {
    use ast::ItemKind as I;
    Some(match &item.kind {
        I::Function(f) => &f.sig.name.name,
        I::Struct(t) | I::Class(t) => &t.name.name,
        I::Enum(e) => &e.name.name,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: FileId = FileId(3);

    fn ident(name: &str, lo: u32) -> ast::Ident {
        ast::Ident {
            name: name.into(),
            span: Span {
                file: FILE,
                lo,
                hi: lo + name.len() as u32,
            },
        }
    }

    /// Scopes by hand: `(lo, hi, names)` each, in the walk's order.
    fn scopes(regions: &[(u32, u32, &[(&str, Shadow)])]) -> Scopes {
        let mut w = Walk::new(FILE);
        for &(lo, hi, names) in regions {
            let s = w.scope();
            w.region(Span { file: FILE, lo, hi }, s);
            for &(n, what) in names {
                w.bind(s, n, what);
            }
        }
        Scopes::from(w)
    }

    use Shadow::{Item, Local};

    #[test]
    fn the_innermost_binding_answers() {
        let s = scopes(&[
            (0, 100, &[("a", Local)]),
            (10, 50, &[("a", Item), ("b", Local)]),
            (60, 90, &[("c", Local)]),
        ]);
        assert_eq!(s.shadow(&ident("a", 20)), Some(Item));
        assert_eq!(s.shadow(&ident("a", 55)), Some(Local));
        assert_eq!(s.shadow(&ident("b", 55)), None);
        assert_eq!(s.shadow(&ident("b", 12)), Some(Local));
        assert_eq!(s.shadow(&ident("c", 70)), Some(Local));
        assert_eq!(s.shadow(&ident("a", 101)), None);
        assert_eq!(s.shadow(&ident("zz", 20)), None);
    }

    /// The last region starting before the name may have ended: the answer is a region around it.
    #[test]
    fn a_name_after_a_nested_region_is_in_the_one_around_it() {
        let s = scopes(&[
            (0, 100, &[("x", Local)]),
            (5, 20, &[]),
            (6, 10, &[("x", Item)]),
            (7, 9, &[]),
        ]);
        assert_eq!(s.shadow(&ident("x", 30)), Some(Local));
        assert_eq!(s.shadow(&ident("x", 7)), Some(Item));
        assert_eq!(s.shadow(&ident("x", 12)), Some(Local));
    }

    /// A function's body and its parameters' scope have one span: both count, the body first.
    #[test]
    fn regions_of_one_span_nest_in_the_walk_s_order() {
        let s = scopes(&[
            (10, 40, &[("p", Local)]),
            (10, 40, &[("p", Item), ("q", Item)]),
        ]);
        assert_eq!(s.shadow(&ident("p", 20)), Some(Item));
        let s = scopes(&[
            (10, 40, &[("p", Local), ("r", Local)]),
            (10, 40, &[("q", Item)]),
        ]);
        assert_eq!(s.shadow(&ident("r", 20)), Some(Local));
        assert_eq!(s.shadow(&ident("q", 20)), Some(Item));
    }

    /// Regions a walk finds out of source order (a class's methods, then its constructor, then its
    /// fields) are sorted first.
    #[test]
    fn regions_out_of_order() {
        let s = scopes(&[
            (50, 60, &[("m", Local)]),
            (10, 20, &[("c", Local)]),
            (30, 40, &[("f", Local)]),
        ]);
        assert_eq!(s.shadow(&ident("m", 55)), Some(Local));
        assert_eq!(s.shadow(&ident("c", 15)), Some(Local));
        assert_eq!(s.shadow(&ident("f", 35)), Some(Local));
        assert_eq!(s.shadow(&ident("c", 35)), None);
        assert_eq!(s.shadow(&ident("m", 45)), None);
    }

    #[test]
    fn another_file_and_no_regions() {
        let s = scopes(&[]);
        assert_eq!(s.shadow(&ident("a", 1)), None);
        let mut w = Walk::new(FILE);
        w.region(
            Span {
                file: FileId(9),
                lo: 0,
                hi: 9,
            },
            0,
        );
        assert!(w.regions.is_empty());
    }
}
