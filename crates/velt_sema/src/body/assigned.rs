//! Which variables code may assign (`x = …`, `x += …`, `x++`), closures included — a syntactic
//! over-approximation, by name. It drops flow narrowing around loops, and finds the variables
//! that closures assign: a call may run such a closure between a check and a use, so those
//! variables are never narrowed (`narrow.rs`, `closure_assigned`).

use std::collections::{HashMap, HashSet};

use velt_common::Span;
use velt_syntax::ast;

/// Assigned names, each with the span of one assignment to it.
pub(crate) type Assigned<'a> = HashMap<&'a str, Span>;

/// Names assigned anywhere in `s`.
pub(crate) fn assigned_in_stmt<'a>(s: &'a ast::Stmt, out: &mut Assigned<'a>) {
    Walk { out, direct: true }.stmt(s);
}

/// Names `e` assigns (also inside closures it creates).
pub(crate) fn assigned_in_expr<'a>(e: &'a ast::Expr, out: &mut Assigned<'a>) {
    Walk { out, direct: true }.expr(e);
}

/// Names that the closures (arrow functions, function expressions, object methods) created in
/// `stmts` and `exprs` assign, other than their own parameters and locals.
pub(crate) fn assigned_by_closures<'a>(
    stmts: &'a [ast::Stmt],
    exprs: impl IntoIterator<Item = &'a ast::Expr>,
) -> Assigned<'a> {
    let mut out = Assigned::new();
    let mut w = Walk {
        out: &mut out,
        direct: false,
    };
    stmts.iter().for_each(|s| w.stmt(s));
    exprs.into_iter().for_each(|e| w.expr(e));
    out
}

/// The body of a closure.
enum Body<'a> {
    Block(&'a ast::Block),
    Expr(&'a ast::Expr),
}

struct Walk<'a, 'o> {
    out: &'o mut Assigned<'a>,
    /// Record assignments outside closures too (not only those closures make).
    direct: bool,
}

impl<'a> Walk<'a, '_> {
    fn stmt(&mut self, s: &'a ast::Stmt) {
        use ast::StmtKind as S;
        match &s.kind {
            S::Var(v) => {
                self.pattern(&v.pattern);
                v.init.iter().for_each(|e| self.expr(e));
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
                init.iter().for_each(|i| self.stmt(i));
                cond.iter().chain(update).for_each(|e| self.expr(e));
                self.block(body);
            }
            S::ForOf {
                pattern,
                iter,
                body,
                ..
            } => {
                self.pattern(pattern);
                self.expr(iter);
                self.block(body);
            }
            S::Block(b) => self.block(b),
            S::Switch {
                discriminant,
                cases,
            } => {
                self.expr(discriminant);
                for c in cases {
                    c.test.iter().for_each(|e| self.expr(e));
                    c.body.iter().for_each(|s| self.stmt(s));
                }
            }
            S::Labeled { body, .. } => self.stmt(body),
            S::Try {
                body,
                catch,
                finally,
            } => {
                self.block(body);
                if let Some((pattern, b)) = catch {
                    pattern.iter().for_each(|p| self.pattern(p));
                    self.block(b);
                }
                finally.iter().for_each(|b| self.block(b));
            }
            // Nested function declarations do not capture (`collect::nested`).
            S::Return(None) | S::Break(_) | S::Continue(_) | S::Item(_) | S::Empty => {}
        }
    }

    fn block(&mut self, b: &'a ast::Block) {
        b.stmts.iter().for_each(|s| self.stmt(s));
    }

    /// The default values in a destructuring pattern (`{ a = (x = 1) }`).
    fn pattern(&mut self, p: &'a ast::Pattern) {
        use ast::PatternKind as P;
        match &p.kind {
            P::Ident(_) | P::Wildcard => {}
            P::Object { fields, .. } => fields.iter().for_each(|(_, p)| self.pattern(p)),
            P::Array { elems, .. } => elems.iter().for_each(|p| self.pattern(p)),
            P::Default { pattern, value } => {
                self.pattern(pattern);
                self.expr(value);
            }
        }
    }

    fn expr(&mut self, e: &'a ast::Expr) {
        use ast::ExprKind as E;
        match &e.kind {
            E::Assign { target, .. } | E::Update { target, .. } => {
                if let (true, E::Ident(id)) = (self.direct, &target.kind) {
                    self.out.entry(id.name.as_str()).or_insert(e.span);
                }
            }
            E::Arrow { params, body, .. } => {
                let names = params.iter().map(|p| p.name.name.as_str());
                let defaults = params.iter().filter_map(|p| p.default.as_ref());
                let body = match body {
                    ast::ArrowBody::Block(b) => Body::Block(b),
                    ast::ArrowBody::Expr(x) => Body::Expr(x),
                };
                return self.closure(names.collect(), defaults.collect(), body);
            }
            E::Function(f) => return self.function(f),
            E::Object(props) => {
                for p in props {
                    if let ast::ObjectProp::Method(f) = p {
                        self.function(f);
                    }
                }
            }
            _ => {}
        }
        crate::ast_walk::children(e, &mut |c| self.expr(c));
    }

    fn function(&mut self, f: &'a ast::FnDecl) {
        let params = &f.sig.params;
        let names = params.iter().map(|p| p.name.name.as_str()).collect();
        let defaults = params.iter().filter_map(|p| p.default.as_ref()).collect();
        self.closure(names, defaults, Body::Block(&f.body));
    }

    /// What a closure assigns, except its parameters and the locals its body declares (at its
    /// top level, before the assignment: an earlier one may name an enclosing variable).
    fn closure(&mut self, params: Vec<&'a str>, defaults: Vec<&'a ast::Expr>, body: Body<'a>) {
        let mut own: HashSet<&'a str> = params.into_iter().collect();
        let mut inner = Assigned::new();
        defaults
            .into_iter()
            .for_each(|d| assigned_in_expr(d, &mut inner));
        match body {
            Body::Expr(x) => assigned_in_expr(x, &mut inner),
            Body::Block(b) => {
                for s in &b.stmts {
                    assigned_in_stmt(s, &mut inner);
                    self.add_free(std::mem::take(&mut inner), &own);
                    if let ast::StmtKind::Var(v) = &s.kind {
                        bound_names(&v.pattern, &mut own);
                    }
                }
            }
        }
        self.add_free(inner, &own);
    }

    fn add_free(&mut self, names: Assigned<'a>, own: &HashSet<&'a str>) {
        for (name, span) in names {
            if !own.contains(name) {
                self.out.entry(name).or_insert(span);
            }
        }
    }
}

/// The names a declaration pattern binds.
fn bound_names<'a>(p: &'a ast::Pattern, out: &mut HashSet<&'a str>) {
    use ast::PatternKind as P;
    match &p.kind {
        P::Ident(id) => {
            out.insert(id.name.as_str());
        }
        P::Wildcard => {}
        P::Object { fields, rest } => {
            fields.iter().for_each(|(_, p)| bound_names(p, out));
            out.extend(rest.iter().map(|r| r.name.as_str()));
        }
        P::Array { elems, rest } => {
            elems.iter().for_each(|p| bound_names(p, out));
            out.extend(rest.iter().map(|r| r.name.as_str()));
        }
        P::Default { pattern, .. } => bound_names(pattern, out),
    }
}

#[cfg(test)]
mod tests;
