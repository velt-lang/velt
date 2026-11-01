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
//! after the declarations of the block's locals it captures. Nested functions calling each
//! other (or one calling itself) form a recursive closure group (`ClosureGroup`), placed
//! together, which sema checks as one (`body::closure_group`). A use before one of those
//! locals is declared (which JS allows when the call runs later), and a group with a generator
//! or with a member returning a value without a written result type, are reported as not
//! supported yet; those declarations stay nested functions, and `kept` keeps sema from
//! reporting each capture again.
//!
//! A nested function reading a class declared in an enclosing function as a value (`return C;`)
//! becomes a closure constant too, though it captures nothing: the class's value is a `const`
//! of the block declaring it (`body::expr::ctor_locals`), which only a closure there reads. So
//! does one naming a type parameter of an enclosing function or class (`function none():
//! Res<T>`): a declaration is collected outside the function, without its type parameters.
//!
//! Declarations that capture nothing, generic ones, overloaded ones and ones using `this` stay
//! nested functions (`collect::nested`).

use rustc_hash::{FxHashMap, FxHashSet};

use velt_common::{Diagnostic, Span};
use velt_syntax::ast::{self, ExprKind as E, ItemKind as I, StmtKind as S};

use crate::SourceModule;

mod graph;
pub(crate) mod scan;

use graph::{cycle, sccs, topo};
use scan::{scan_fn, scan_stmt, Scan};

/// The modules with their capturing nested functions rewritten into closure constants.
pub(crate) struct Rewritten {
    pub modules: Vec<SourceModule>,
    pub diags: Vec<Diagnostic>,
    /// Name spans of the capturing nested functions left as declarations (reported here).
    pub kept: FxHashSet<Span>,
    /// The recursive closure groups made.
    pub groups: Vec<ClosureGroup>,
}

/// Nested functions of one block that call each other (or one calling itself), made closure
/// constants that refer to each other: sema checks them together, each capturing every
/// variable of `captures` in that order, so they share one environment layout and a member
/// makes another from its own environment (`Intrinsic::ClosureSibling`).
#[derive(Clone, Debug)]
pub(crate) struct ClosureGroup {
    /// The members in source order.
    pub names: Vec<String>,
    /// Their name spans (the `const` patterns).
    pub name_spans: Vec<Span>,
    /// The spans of their arrow functions.
    pub arrow_spans: Vec<Span>,
    /// The enclosing variables and closure constants the members use, sorted.
    pub captures: Vec<String>,
    /// The members' arrow functions, as rewritten (sema checks them in trials before the
    /// statements declaring them).
    pub arrows: Vec<ast::Expr>,
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
                rw.item(i, &FxHashSet::default());
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
        groups: rw.groups,
    })
}

#[derive(Default)]
struct Rw {
    diags: Vec<Diagnostic>,
    kept: FxHashSet<Span>,
    groups: Vec<ClosureGroup>,
}

/// A nested function declaration of a block that may become a closure constant.
struct Cand {
    /// Its statement's index in the block.
    at: usize,
    name: ast::Ident,
    /// The names it uses that it does not bind itself.
    free: FxHashSet<String>,
    /// Does it read a class of an enclosing function as a value, or name a type parameter of
    /// an enclosing function or class (module docs)? It becomes a closure constant though it
    /// captures nothing.
    encloses: bool,
}

/// The entry of `locals` standing for class `name` declared in an enclosing function (no
/// variable is named so), which a nested function reading it as a value captures.
fn class_key(name: &str) -> String {
    format!("#class {name}")
}

/// The key in `locals` of a type parameter of an enclosing function or class: a nested
/// function naming it becomes a closure constant, which sees it (a declaration is collected
/// outside the function, without its type parameters).
fn type_key(name: &str) -> String {
    format!("#type {name}")
}

/// `locals` with the type parameters `generics` added (`type_key`).
fn with_generics<'a>(
    locals: &'a FxHashSet<String>,
    generics: &[ast::GenericParam],
) -> std::borrow::Cow<'a, FxHashSet<String>> {
    if generics.is_empty() {
        return std::borrow::Cow::Borrowed(locals);
    }
    let mut l = locals.clone();
    l.extend(generics.iter().map(|g| type_key(&g.name.name)));
    std::borrow::Cow::Owned(l)
}

/// The single-segment type names function `f` writes anywhere (`T` of `Res<T>`).
fn type_names(f: &ast::FnDecl) -> FxHashSet<String> {
    struct Names(FxHashSet<String>);
    impl<'a> velt_syntax::visit::Visit<'a> for Names {
        fn ty(&mut self, t: &'a ast::TypeExpr) {
            if let ast::TypeExprKind::Named { path, .. } = &t.kind {
                if let [one] = path.as_slice() {
                    self.0.insert(one.name.clone());
                }
            }
        }
    }
    let mut v = Names(FxHashSet::default());
    velt_syntax::visit::walk_fn(f, &mut v);
    v.0
}

impl Rw {
    /// Every function body in `item`; `outer`: the locals of the functions around it.
    fn item(&mut self, item: &mut ast::Item, outer: &FxHashSet<String>) {
        match &mut item.kind {
            I::Function(f) => self.func(f, outer),
            I::Struct(t) | I::Class(t) => {
                let outer = with_generics(outer, &t.generics).into_owned();
                for e in t.fields.iter_mut().filter_map(|f| f.default.as_mut()) {
                    self.expr(e, &outer);
                }
                if let Some(c) = &mut t.constructor {
                    self.func(c, &outer);
                }
                for m in &mut t.methods {
                    self.func(&mut m.decl, &outer);
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
            I::Namespace(n) => n.items.iter_mut().for_each(|i| self.item(i, outer)),
            I::Import(_) | I::Enum(_) | I::TypeAlias(_) | I::ExternFn(_) => {}
        }
    }

    fn func(&mut self, f: &mut ast::FnDecl, outer: &FxHashSet<String>) {
        let generics = f.sig.generics.clone();
        let outer = &*with_generics(outer, &generics);
        let params = f.sig.params.clone();
        for e in f.sig.params.iter_mut().filter_map(|p| p.default.as_mut()) {
            self.expr(e, outer);
        }
        self.body(&params, &mut f.body, outer);
    }

    /// A function body: its locals (parameters and every variable bound inside, at any
    /// depth) join `outer` for the nested functions in it.
    fn body(&mut self, params: &[ast::Param], b: &mut ast::Block, outer: &FxHashSet<String>) {
        let mut s = Scan::default();
        s.params(params);
        s.block(b);
        if !s.nested_fn {
            return; // no nested function at any depth
        }
        let mut locals = outer.clone();
        locals.extend(s.binds);
        locals.extend(s.classes.iter().map(|c| class_key(c)));
        self.block(&mut b.stmts, &locals);
    }

    /// The statements of a block: its capturing nested functions become closure constants,
    /// placed as described in the module docs; then everything inside is rewritten.
    fn block(&mut self, stmts: &mut Vec<ast::Stmt>, locals: &FxHashSet<String>) {
        let mut locals = std::borrow::Cow::Borrowed(locals);
        let groups_before = self.groups.len();
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
        // The arrows of the groups made here, once everything inside them is rewritten.
        for g in &mut self.groups[groups_before..] {
            if !g.arrows.is_empty() {
                continue;
            }
            g.arrows = g
                .name_spans
                .iter()
                .map(|span| {
                    stmts
                        .iter()
                        .find_map(|s| match &s.kind {
                            S::Var(v) if v.pattern.span == *span => v.init.clone(),
                            _ => None,
                        })
                        .expect("ICE: a group member's constant")
                })
                .collect();
        }
    }

    /// Rewrite the capturing nested functions of `stmts`; the names of those rewritten.
    fn convert(&mut self, stmts: &mut Vec<ast::Stmt>, locals: &FxHashSet<String>) -> Vec<String> {
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
            let encloses = sc
                .values
                .iter()
                .filter(|n| !sc.binds.contains(*n) && !sc.classes.contains(*n))
                .any(|n| locals.contains(&class_key(n)))
                || type_names(f).iter().any(|n| locals.contains(&type_key(n)));
            let free = sc
                .refs
                .into_iter()
                .filter(|n| !sc.binds.contains(n))
                .collect();
            cands.push(Cand {
                at,
                name: f.sig.name.clone(),
                free,
                encloses,
            });
        }
        // Those capturing a local, then those using one of those (a closure constant).
        let mut conv: Vec<bool> = cands
            .iter()
            .map(|c| c.encloses || c.free.iter().any(|n| locals.contains(n)))
            .collect();
        loop {
            let names: FxHashSet<&str> = cands
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
        let edges: FxHashMap<usize, Vec<usize>> = idx.iter().map(|&a| (a, uses_of(a))).collect();
        // Recursion through closures: each set of functions calling each other becomes a
        // recursive closure group (`ClosureGroup`); the shapes it does not cover stay
        // declarations, reported.
        let mut keep = FxHashSet::default();
        let mut group_of: FxHashMap<usize, usize> = FxHashMap::default();
        let mut recursive: Vec<Vec<usize>> = vec![];
        for g in sccs(&idx, &edges) {
            let cyclic = g.len() > 1 || edges[&g[0]].contains(&g[0]);
            if !cyclic {
                continue;
            }
            if let Some(why) = g.iter().find_map(|&a| group_misfit(stmts, &cands[a])) {
                for &a in &g {
                    keep.insert(a);
                    if let Some(path) = cycle(a, &edges) {
                        self.report_recursion(&cands, a, &path, &why);
                    }
                }
                continue;
            }
            for &a in &g {
                group_of.insert(a, recursive.len());
            }
            recursive.push(g);
        }
        let idx: Vec<usize> = idx.into_iter().filter(|a| !keep.contains(a)).collect();
        // A group is placed as one: its members' uses, positions and captured locals joined.
        let node = |a: usize| group_of.get(&a).map_or(a, |&g| recursive[g][0]);
        let members = |n: usize| -> Vec<usize> {
            match group_of.get(&n) {
                Some(&g) => recursive[g].clone(),
                None => vec![n],
            }
        };
        let nodes: Vec<usize> = idx.iter().copied().filter(|&a| node(a) == a).collect();
        let node_edges: FxHashMap<usize, Vec<usize>> = nodes
            .iter()
            .map(|&n| {
                let mut out: Vec<usize> = vec![];
                for m in members(n) {
                    for &b in &edges[&m] {
                        let b = node(b);
                        if b != n && !keep.contains(&b) && !out.contains(&b) {
                            out.push(b);
                        }
                    }
                }
                (n, out)
            })
            .collect();
        // Where each one goes: before the first statement using it, at most where it is
        // declared, after the block's locals it captures.
        let n = stmts.len();
        let cand_at: FxHashSet<usize> = idx.iter().map(|&a| cands[a].at).collect();
        let stmt_refs: Vec<Option<FxHashSet<String>>> = stmts
            .iter()
            .enumerate()
            .map(|(j, s)| {
                (!cand_at.contains(&j) && !matches!(s.kind, S::Item(_))).then(|| scan_stmt(s).refs)
            })
            .collect();
        let mut declared: FxHashMap<String, usize> = FxHashMap::default();
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
        let order = topo(&nodes, &node_edges);
        let mut pos: FxHashMap<usize, usize> = FxHashMap::default();
        let mut bad = FxHashSet::default();
        for &a in &order {
            let ms = members(a);
            let first_use = stmt_refs
                .iter()
                .position(|r| {
                    r.as_ref()
                        .is_some_and(|r| ms.iter().any(|&m| r.contains(&cands[m].name.name)))
                })
                .unwrap_or(n);
            let by_users = nodes
                .iter()
                .filter(|u| node_edges[u].contains(&a))
                .filter_map(|u| pos.get(u).copied())
                .min()
                .unwrap_or(n);
            let hi = first_use.min(by_users);
            let after = ms
                .iter()
                .flat_map(|&m| cands[m].free.iter())
                .filter(|v| locals.contains(*v))
                .filter_map(|v| declared.get(v).map(|&j| (j + 1, v)))
                .max();
            let lo = after.map_or(0, |(j, _)| j);
            let at = ms
                .iter()
                .map(|&m| cands[m].at)
                .min()
                .expect("ICE: empty group");
            let p = lo.max(at.min(hi));
            if p > hi {
                let (_, var) = after.expect("ICE: a lower bound comes from a local");
                let c = &cands[ms[0]];
                self.report_early_use(c, var);
                bad.extend(ms.iter().copied());
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
        // The recursive groups left: what each captures (the locals and closure constants its
        // members use), in one order for all members.
        let converted: FxHashSet<&str> = idx.iter().map(|&a| cands[a].name.name.as_str()).collect();
        for ms in recursive
            .iter()
            .filter(|ms| !ms.iter().any(|a| bad.contains(a)))
        {
            let own: FxHashSet<&str> = ms.iter().map(|&m| cands[m].name.name.as_str()).collect();
            let captures: std::collections::BTreeSet<String> = ms
                .iter()
                .flat_map(|&m| cands[m].free.iter())
                .filter(|v| !own.contains(v.as_str()))
                .filter(|v| locals.contains(*v) || converted.contains(v.as_str()))
                .cloned()
                .collect();
            let mut sorted = ms.clone();
            sorted.sort_by_key(|&m| cands[m].at);
            self.groups.push(ClosureGroup {
                names: sorted.iter().map(|&m| cands[m].name.name.clone()).collect(),
                name_spans: sorted.iter().map(|&m| cands[m].name.span).collect(),
                arrow_spans: sorted
                    .iter()
                    .map(|&m| item_span(&stmts[cands[m].at]))
                    .collect(),
                captures: captures.into_iter().collect(),
                arrows: vec![],
            });
        }
        // Emit: at each position, the closures used by others first (a group's members in
        // source order).
        let mut at_pos: FxHashMap<usize, Vec<usize>> = FxHashMap::default();
        for &a in order.iter().rev() {
            if idx.contains(&a) {
                let mut ms = members(a);
                ms.sort_by_key(|&m| cands[m].at);
                at_pos.entry(pos[&a]).or_default().extend(ms);
            }
        }
        let moved: FxHashMap<usize, usize> = idx.iter().map(|&a| (cands[a].at, a)).collect();
        let mut old: Vec<Option<ast::Stmt>> = std::mem::take(stmts).into_iter().map(Some).collect();
        let mut made: FxHashMap<usize, ast::Stmt> = FxHashMap::default();
        for (&at, &a) in &moved {
            let s = old[at].take().expect("ICE: statement taken twice");
            made.insert(a, closure_const(s, group_of.contains_key(&a)));
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

    fn report_recursion(
        &mut self,
        cands: &[Cand],
        a: usize,
        path: &[usize],
        (why, fix): &(String, String),
    ) {
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
            .with_note(why.clone())
            .with_note(fix.clone()),
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

    fn stmt(&mut self, s: &mut ast::Stmt, locals: &FxHashSet<String>) {
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

    fn expr(&mut self, e: &mut ast::Expr, locals: &FxHashSet<String>) {
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
                    inner.extend(s.classes.iter().map(|c| class_key(c)));
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

/// Why the member `c` of a set of nested functions calling each other cannot join a recursive
/// closure group, if it cannot (a generator, or a result type that is neither written nor
/// `void`), and what to do instead.
fn group_misfit(stmts: &[ast::Stmt], c: &Cand) -> Option<(String, String)> {
    let S::Item(item) = &stmts[c.at].kind else {
        return None;
    };
    let I::Function(f) = &item.kind else {
        return None;
    };
    let name = &c.name.name;
    if f.sig.is_generator {
        return Some((
            format!("`{name}` is a generator, and a generator function value cannot refer to itself yet"),
            "pass the locals it uses as parameters, or declare it at module level".into(),
        ));
    }
    if f.sig.ret.is_none() && scan::returns_value(&f.body) {
        return Some((
            format!("`{name}` returns a value but its result type is not written: functions calling each other through closures are typed before their bodies are checked"),
            format!("write its result type: `function {name}(...): T`"),
        ));
    }
    None
}

/// The span of a declaration statement's item (the span of its closure constant's arrow).
fn item_span(s: &ast::Stmt) -> Span {
    match &s.kind {
        S::Item(item) => item.span,
        _ => s.span,
    }
}

/// The statement of a nested function declaration as its closure constant. A member of a
/// recursive group gets its result type written when it returns nothing (`void`, or
/// `Promise<void>` when async): the group's function types are known before their bodies.
fn closure_const(s: ast::Stmt, in_group: bool) -> ast::Stmt {
    let S::Item(item) = s.kind else {
        unreachable!("ICE: not a declaration")
    };
    let I::Function(f) = item.kind else {
        unreachable!("ICE: not a function declaration")
    };
    let name = f.sig.name.clone();
    let mut f = f;
    if in_group && f.sig.ret.is_none() {
        let at = Span::new(name.span.file, name.span.hi, name.span.hi);
        let named = |n: &str, args: Vec<ast::TypeExpr>| ast::TypeExpr {
            kind: ast::TypeExprKind::Named {
                path: vec![ast::Ident {
                    name: n.into(),
                    span: at,
                }],
                args,
            },
            span: at,
        };
        let void = named("void", vec![]);
        f.sig.ret = Some(match f.sig.is_async {
            true => named("Promise", vec![void]),
            false => void,
        });
    }
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
