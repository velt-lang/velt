//! Nested function declarations that use the enclosing function's locals
//! (docs/reference/functions.md "Declarations").
//!
//! In JS such a declaration is a closure, hoisted to the start of its block. Before collection,
//! each one becomes the closure constant `const name = (params): R => { body };` (a generator
//! `const name = function* (params) { body };`), so every later pass sees an ordinary closure:
//! it captures by reference as any closure does (a `const` only called borrows, one used as a
//! value keeps the variables it assigns in shared cells). A nested function that calls one of
//! these becomes one too, as it captures that closure.
//!
//! Hoisting: the constant goes where the declaration is, or, when the block uses the name
//! earlier (a call, or a closure referring to it), right before the first statement using it,
//! after the declarations of the block's locals it captures. A use before one of those locals
//! is declared (which JS allows when the call runs later), and recursion through closures (a
//! closure cannot refer to itself yet), are reported as not supported yet; those declarations
//! stay nested functions, and `kept` keeps sema from reporting each capture again.
//!
//! Declarations that capture nothing, generic ones, overloaded ones and ones using `this` stay
//! nested functions (`collect::nested`).

use std::collections::{HashMap, HashSet};

use velt_common::{Diagnostic, Span};
use velt_syntax::ast::{self, ExprKind as E, ItemKind as I, StmtKind as S};

use crate::SourceModule;

mod scan;

use scan::{scan_fn, scan_stmt, Scan};

/// The modules with their capturing nested functions rewritten into closure constants.
pub(crate) struct Rewritten {
    pub modules: Vec<SourceModule>,
    pub diags: Vec<Diagnostic>,
    /// Name spans of the capturing nested functions left as declarations (reported here).
    pub kept: HashSet<Span>,
}

/// The rewritten modules, or `None` when no function body declares a nested function.
pub(crate) fn rewrite(modules: &[SourceModule]) -> Option<Rewritten> {
    let any = modules.iter().any(|m| {
        m.ast.items.iter().any(|i| {
            let mut s = Scan::default();
            s.item_bodies(i);
            s.nested_fn
        })
    });
    if !any {
        return None;
    }
    let mut rw = Rw::default();
    let modules = modules
        .iter()
        .map(|m| {
            let mut items = m.ast.items.clone();
            for i in &mut items {
                rw.item(i, &HashSet::new());
            }
            SourceModule {
                path: m.path.clone(),
                is_std: m.is_std,
                file: m.file,
                ast: ast::Module {
                    items,
                    span: m.ast.span,
                    jsx_import_source: m.ast.jsx_import_source.clone(),
                },
                imports: m.imports.clone(),
                jsx_runtime: m.jsx_runtime.clone(),
            }
        })
        .collect();
    Some(Rewritten {
        modules,
        diags: rw.diags,
        kept: rw.kept,
    })
}

#[derive(Default)]
struct Rw {
    diags: Vec<Diagnostic>,
    kept: HashSet<Span>,
}

/// A nested function declaration of a block that may become a closure constant.
struct Cand {
    /// Its statement's index in the block.
    at: usize,
    name: ast::Ident,
    /// The names it uses that it does not bind itself.
    free: HashSet<String>,
}

impl Rw {
    /// Every function body in `item`; `outer`: the locals of the functions around it.
    fn item(&mut self, item: &mut ast::Item, outer: &HashSet<String>) {
        match &mut item.kind {
            I::Function(f) => self.func(f, outer),
            I::Struct(t) | I::Class(t) => {
                for e in t.fields.iter_mut().filter_map(|f| f.default.as_mut()) {
                    self.expr(e, outer);
                }
                if let Some(c) = &mut t.constructor {
                    self.func(c, outer);
                }
                for m in &mut t.methods {
                    self.func(&mut m.decl, outer);
                }
            }
            I::Extend(e) => {
                for m in &mut e.methods {
                    self.func(&mut m.decl, outer);
                }
            }
            I::Interface(i) => {
                for b in i.methods.iter_mut().filter_map(|m| m.body.as_mut()) {
                    self.body(&[], b, outer);
                }
            }
            I::Var(v) => {
                if let Some(e) = &mut v.init {
                    self.expr(e, outer);
                }
            }
            I::Augment(a) => a.items.iter_mut().for_each(|i| self.item(i, outer)),
            I::Import(_) | I::Enum(_) | I::TypeAlias(_) | I::ExternFn(_) => {}
        }
    }

    fn func(&mut self, f: &mut ast::FnDecl, outer: &HashSet<String>) {
        let params = f.sig.params.clone();
        for e in f.sig.params.iter_mut().filter_map(|p| p.default.as_mut()) {
            self.expr(e, outer);
        }
        self.body(&params, &mut f.body, outer);
    }

    /// A function body: its locals (parameters and every variable bound inside, at any
    /// depth) join `outer` for the nested functions in it.
    fn body(&mut self, params: &[ast::Param], b: &mut ast::Block, outer: &HashSet<String>) {
        let mut s = Scan::default();
        s.params(params);
        s.block(b);
        if !s.nested_fn {
            return; // no nested function at any depth
        }
        let mut locals = outer.clone();
        locals.extend(s.binds);
        self.block(&mut b.stmts, &locals);
    }

    /// The statements of a block: its capturing nested functions become closure constants,
    /// placed as described in the module docs; then everything inside is rewritten.
    fn block(&mut self, stmts: &mut Vec<ast::Stmt>, locals: &HashSet<String>) {
        let mut locals = std::borrow::Cow::Borrowed(locals);
        if stmts
            .iter()
            .any(|s| matches!(&s.kind, S::Item(i) if matches!(i.kind, I::Function(_))))
        {
            let converted = self.convert(stmts, &locals);
            if !converted.is_empty() {
                locals.to_mut().extend(converted);
            }
        }
        for s in stmts.iter_mut() {
            self.stmt(s, &locals);
        }
    }

    /// Rewrite the capturing nested functions of `stmts`; the names of those rewritten.
    fn convert(&mut self, stmts: &mut Vec<ast::Stmt>, locals: &HashSet<String>) -> Vec<String> {
        let mut cands: Vec<Cand> = vec![];
        for (at, s) in stmts.iter().enumerate() {
            let S::Item(item) = &s.kind else { continue };
            let I::Function(f) = &item.kind else { continue };
            if !f.sig.generics.is_empty() || !f.overloads.is_empty() {
                continue;
            }
            let sc = scan_fn(f);
            if sc.this {
                continue;
            }
            let free = sc
                .refs
                .into_iter()
                .filter(|n| !sc.binds.contains(n))
                .collect();
            cands.push(Cand {
                at,
                name: f.sig.name.clone(),
                free,
            });
        }
        // Those capturing a local, then those using one of those (a closure constant).
        let mut conv: Vec<bool> = cands
            .iter()
            .map(|c| c.free.iter().any(|n| locals.contains(n)))
            .collect();
        loop {
            let names: HashSet<&str> = cands
                .iter()
                .zip(&conv)
                .filter(|(_, &v)| v)
                .map(|(c, _)| c.name.name.as_str())
                .collect();
            let mut changed = false;
            for (i, c) in cands.iter().enumerate() {
                if !conv[i] && c.free.iter().any(|n| names.contains(n.as_str())) {
                    conv[i] = true;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        let idx: Vec<usize> = (0..cands.len()).filter(|&i| conv[i]).collect();
        if idx.is_empty() {
            return vec![];
        }
        // Uses between them: `edges[a]` holds the candidates `a` uses.
        let uses_of = |a: usize| -> Vec<usize> {
            idx.iter()
                .copied()
                .filter(|&b| cands[a].free.contains(&cands[b].name.name))
                .collect()
        };
        let edges: HashMap<usize, Vec<usize>> = idx.iter().map(|&a| (a, uses_of(a))).collect();
        // Recursion through closures: not supported yet; those stay declarations.
        let mut keep = HashSet::new();
        for &a in &idx {
            if let Some(path) = cycle(a, &edges) {
                keep.insert(a);
                self.report_recursion(&cands, a, &path);
            }
        }
        let idx: Vec<usize> = idx.into_iter().filter(|a| !keep.contains(a)).collect();
        // Where each one goes: before the first statement using it, at most where it is
        // declared, after the block's locals it captures.
        let n = stmts.len();
        let cand_at: HashSet<usize> = idx.iter().map(|&a| cands[a].at).collect();
        let stmt_refs: Vec<Option<HashSet<String>>> = stmts
            .iter()
            .enumerate()
            .map(|(j, s)| {
                (!cand_at.contains(&j) && !matches!(s.kind, S::Item(_))).then(|| scan_stmt(s).refs)
            })
            .collect();
        let mut declared: HashMap<String, usize> = HashMap::new();
        for (j, s) in stmts.iter().enumerate() {
            if let S::Var(v) = &s.kind {
                let mut sc = Scan::default();
                sc.pattern(&v.pattern);
                for b in sc.binds {
                    declared.entry(b).or_insert(j);
                }
            }
        }
        // Users first, so a closure's position bounds those it uses.
        let order = topo(&idx, &edges);
        let mut pos: HashMap<usize, usize> = HashMap::new();
        let mut bad = HashSet::new();
        for &a in &order {
            let c = &cands[a];
            let first_use = stmt_refs
                .iter()
                .position(|r| r.as_ref().is_some_and(|r| r.contains(&c.name.name)))
                .unwrap_or(n);
            let by_users = idx
                .iter()
                .filter(|u| edges[u].contains(&a))
                .filter_map(|u| pos.get(u).copied())
                .min()
                .unwrap_or(n);
            let hi = first_use.min(by_users);
            let after = c
                .free
                .iter()
                .filter(|v| locals.contains(*v))
                .filter_map(|v| declared.get(v).map(|&j| (j + 1, v)))
                .max();
            let lo = after.map_or(0, |(j, _)| j);
            let p = lo.max(c.at.min(hi));
            if p > hi {
                let (_, var) = after.expect("ICE: a lower bound comes from a local");
                self.report_early_use(c, var);
                bad.insert(a);
            }
            pos.insert(a, p);
        }
        let idx: Vec<usize> = idx.into_iter().filter(|a| !bad.contains(a)).collect();
        for &a in keep.iter().chain(&bad) {
            self.kept.insert(cands[a].name.span);
        }
        if idx.is_empty() {
            return vec![];
        }
        // Emit: at each position, the closures used by others first.
        let mut at_pos: HashMap<usize, Vec<usize>> = HashMap::new();
        for &a in order.iter().rev() {
            if idx.contains(&a) {
                at_pos.entry(pos[&a]).or_default().push(a);
            }
        }
        let moved: HashMap<usize, usize> = idx.iter().map(|&a| (cands[a].at, a)).collect();
        let mut old: Vec<Option<ast::Stmt>> = std::mem::take(stmts).into_iter().map(Some).collect();
        let mut made: HashMap<usize, ast::Stmt> = HashMap::new();
        for (&at, &a) in &moved {
            let s = old[at].take().expect("ICE: statement taken twice");
            made.insert(a, closure_const(s));
        }
        for (j, s) in old.into_iter().enumerate() {
            for a in at_pos.remove(&j).unwrap_or_default() {
                stmts.push(made.remove(&a).expect("ICE: closure emitted twice"));
            }
            if let Some(s) = s {
                stmts.push(s);
            }
        }
        idx.iter().map(|&a| cands[a].name.name.clone()).collect()
    }

    fn report_recursion(&mut self, cands: &[Cand], a: usize, path: &[usize]) {
        let c = &cands[a];
        let through = match path {
            [] => String::new(),
            p => format!(
                " through {}",
                p.iter()
                    .map(|&b| format!("`{}`", cands[b].name.name))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        };
        self.diags.push(
            Diagnostic::error(
                format!(
                    "`{}` uses local variables of the enclosing function and calls itself{through}: not supported yet",
                    c.name.name
                ),
                c.name.span,
            )
            .with_note("a nested function using the enclosing function's locals is a closure, and a closure cannot refer to itself yet")
            .with_note("pass the locals it uses as parameters, or declare it at module level"),
        );
    }

    fn report_early_use(&mut self, c: &Cand, var: &str) {
        self.diags.push(
            Diagnostic::error(
                format!(
                    "`{}` is used before `{var}`, a local it uses, is declared: not supported yet",
                    c.name.name
                ),
                c.name.span,
            )
            .with_note("JS allows this when the function runs after `{var}` is set; declare `{var}` before the first use of the function".replace("{var}", var)),
        );
    }

    fn stmt(&mut self, s: &mut ast::Stmt, locals: &HashSet<String>) {
        match &mut s.kind {
            S::Var(v) => {
                if let Some(e) = &mut v.init {
                    self.expr(e, locals);
                }
            }
            S::Expr(e) | S::Throw(e) | S::Return(Some(e)) => self.expr(e, locals),
            S::If { cond, then, els } => {
                self.expr(cond, locals);
                self.block(&mut then.stmts, locals);
                if let Some(e) = els {
                    self.stmt(e, locals);
                }
            }
            S::While { cond, body } | S::DoWhile { body, cond } => {
                self.expr(cond, locals);
                self.block(&mut body.stmts, locals);
            }
            S::For {
                init,
                cond,
                update,
                body,
            } => {
                if let Some(i) = init {
                    self.stmt(i, locals);
                }
                for e in cond.iter_mut().chain(update.iter_mut()) {
                    self.expr(e, locals);
                }
                self.block(&mut body.stmts, locals);
            }
            S::ForOf { iter, body, .. } => {
                self.expr(iter, locals);
                self.block(&mut body.stmts, locals);
            }
            S::Block(b) => self.block(&mut b.stmts, locals),
            S::Labeled { body, .. } => self.stmt(body, locals),
            S::Switch {
                discriminant,
                cases,
            } => {
                self.expr(discriminant, locals);
                for c in cases {
                    if let Some(t) = &mut c.test {
                        self.expr(t, locals);
                    }
                    self.block(&mut c.body, locals);
                }
            }
            S::Try {
                body,
                catch,
                finally,
            } => {
                self.block(&mut body.stmts, locals);
                if let Some((_, c)) = catch {
                    self.block(&mut c.stmts, locals);
                }
                if let Some(f) = finally {
                    self.block(&mut f.stmts, locals);
                }
            }
            S::Item(item) => self.item(item, locals),
            S::Return(None) | S::Break(_) | S::Continue(_) | S::Empty => {}
        }
    }

    fn expr(&mut self, e: &mut ast::Expr, locals: &HashSet<String>) {
        match &mut e.kind {
            E::Arrow {
                params,
                body: ast::ArrowBody::Block(b),
                ..
            } => {
                for p in params.iter_mut().filter_map(|p| p.default.as_mut()) {
                    self.expr(p, locals);
                }
                // Its own locals join (a body at module level has no enclosing function).
                let mut s = Scan::default();
                for p in params.iter() {
                    match &p.pattern {
                        Some(q) => s.pattern(q),
                        None => {
                            s.binds.insert(p.name.name.clone());
                        }
                    }
                }
                s.block(b);
                if s.nested_fn {
                    let mut inner = locals.clone();
                    inner.extend(s.binds);
                    self.block(&mut b.stmts, &inner);
                }
            }
            E::Function(d) => self.func(d, locals),
            E::Object(props) => {
                for p in props {
                    if let ast::ObjectProp::Method(d) = p {
                        self.func(d, locals);
                    }
                }
            }
            _ => {}
        }
        crate::ast_walk::children_mut(e, &mut |c| self.expr(c, locals));
    }
}

/// A path from `a` back to itself through `edges` (the nodes after `a`, empty for a direct
/// self-use), if there is one.
fn cycle(a: usize, edges: &HashMap<usize, Vec<usize>>) -> Option<Vec<usize>> {
    if edges[&a].contains(&a) {
        return Some(vec![]);
    }
    // Breadth-first, remembering where each node was reached from.
    let mut from: HashMap<usize, usize> = HashMap::new();
    let mut queue: std::collections::VecDeque<usize> = edges[&a].iter().copied().collect();
    for &b in &edges[&a] {
        from.insert(b, a);
    }
    while let Some(b) = queue.pop_front() {
        for &c in &edges[&b] {
            if c == a {
                let mut path = vec![b];
                let mut at = b;
                while from[&at] != a {
                    at = from[&at];
                    path.push(at);
                }
                path.reverse();
                return Some(path);
            }
            if let std::collections::hash_map::Entry::Vacant(v) = from.entry(c) {
                v.insert(b);
                queue.push_back(c);
            }
        }
    }
    None
}

/// `nodes` ordered so that a node comes before every node it uses (`edges`, acyclic among
/// `nodes`), keeping source order otherwise.
fn topo(nodes: &[usize], edges: &HashMap<usize, Vec<usize>>) -> Vec<usize> {
    let mut users: HashMap<usize, usize> = nodes.iter().map(|&a| (a, 0)).collect();
    for &a in nodes {
        for b in &edges[&a] {
            if let Some(k) = users.get_mut(b) {
                *k += 1;
            }
        }
    }
    let mut out = vec![];
    let mut done = HashSet::new();
    while out.len() < nodes.len() {
        let Some(&a) = nodes.iter().find(|a| !done.contains(*a) && users[*a] == 0) else {
            // Only cycles are left (cannot happen: they were kept as declarations).
            out.extend(nodes.iter().filter(|a| !done.contains(*a)));
            break;
        };
        done.insert(a);
        out.push(a);
        for b in &edges[&a] {
            if let Some(k) = users.get_mut(b) {
                *k -= 1;
            }
        }
    }
    out
}

/// The statement of a nested function declaration as its closure constant.
fn closure_const(s: ast::Stmt) -> ast::Stmt {
    let S::Item(item) = s.kind else {
        unreachable!("ICE: not a declaration")
    };
    let I::Function(f) = item.kind else {
        unreachable!("ICE: not a function declaration")
    };
    let name = f.sig.name.clone();
    let init = if f.sig.is_generator {
        let mut f = f;
        f.sig.name.name = String::new();
        E::Function(Box::new(f))
    } else {
        E::Arrow {
            type_params: vec![],
            params: f
                .sig
                .params
                .into_iter()
                .map(|p| ast::ArrowParam {
                    name: p.name,
                    pattern: p.pattern,
                    ty: Some(p.ty),
                    default: p.default,
                    optional: p.optional,
                    rest: p.rest,
                })
                .collect(),
            ret: f.sig.ret,
            throws: f.sig.throws,
            body: ast::ArrowBody::Block(f.body),
            is_async: f.sig.is_async,
        }
    };
    let pattern = ast::Pattern {
        id: ast::NodeId(u32::MAX),
        kind: ast::PatternKind::Ident(name.clone()),
        span: name.span,
    };
    let var = ast::VarDecl {
        kind: ast::VarKind::Const,
        pattern,
        ty: None,
        init: Some(ast::Expr {
            id: ast::NodeId(u32::MAX),
            kind: init,
            span: item.span,
        }),
        span: s.span,
    };
    ast::Stmt {
        kind: S::Var(var),
        span: s.span,
    }
}
