//! Which variables code may assign (`x = …`, `x += …`, `x++`), closures included — a syntactic
//! over-approximation, by name. It drops flow narrowing around loops, and finds the variables
//! that closures assign: a call may run such a closure between a check and a use, so those
//! variables are never narrowed (`narrow.rs`, `closure_assigned`).

use rustc_hash::{FxHashMap, FxHashSet};

use velt_common::Span;
use velt_syntax::ast;

/// Assigned names, each with the span of one assignment to it.
pub(crate) type Assigned<'a> = FxHashMap<&'a str, Span>;

/// Names assigned anywhere in `s`.
pub(crate) fn assigned_in_stmt<'a>(s: &'a ast::Stmt, out: &mut Assigned<'a>) {
    Walk {
        out,
        direct: true,
        fields: false,
        paths: None,
    }
    .stmt(s);
}

/// Names `e` assigns (also inside closures it creates).
pub(crate) fn assigned_in_expr<'a>(e: &'a ast::Expr, out: &mut Assigned<'a>) {
    Walk {
        out,
        direct: true,
        fields: false,
        paths: None,
    }
    .expr(e);
}

/// The member and index targets `s` assigns or deletes outside closures (`o.v = 5`,
/// `r[k] = x`, `delete r[k]`): a loop drops their narrowing (`field_narrow`).
pub(crate) fn written_paths<'a>(s: &'a ast::Stmt, out: &mut Vec<&'a ast::Expr>) {
    let mut names = Assigned::default();
    Walk {
        out: &mut names,
        direct: true,
        fields: false,
        paths: Some(out),
    }
    .stmt(s);
}

/// Names that the closures (arrow functions, function expressions, object methods) created in
/// `stmts` and `exprs` assign, other than their own parameters and locals.
pub(crate) fn assigned_by_closures<'a>(
    stmts: &'a [ast::Stmt],
    exprs: impl IntoIterator<Item = &'a ast::Expr>,
) -> Assigned<'a> {
    let mut out = Assigned::default();
    let mut w = Walk {
        out: &mut out,
        direct: false,
        fields: false,
        paths: None,
    };
    stmts.iter().for_each(|s| w.stmt(s));
    exprs.into_iter().for_each(|e| w.expr(e));
    out
}

/// The fields of `this` that `stmts` assign (`this.x = …`) outside closures: a constructor's
/// assignments (a closure cannot assign a `readonly` field).
pub(crate) fn this_fields_assigned(stmts: &[ast::Stmt]) -> Assigned<'_> {
    let mut out = Assigned::default();
    let mut w = Walk {
        out: &mut out,
        direct: false,
        fields: true,
        paths: None,
    };
    stmts.iter().for_each(|s| w.stmt(s));
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
    /// Record the fields of `this` assigned outside closures, not variables.
    fields: bool,
    /// Also collect the member and index targets written outside closures.
    paths: Option<&'o mut Vec<&'a ast::Expr>>,
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
            E::Arrow { .. } | E::Function(_) if self.fields => return,
            E::Assign { target, .. } | E::Update { target, .. } if self.fields => {
                if let E::Member { object, prop, .. } = &target.kind {
                    if matches!(object.kind, E::This) {
                        self.out.entry(prop.name.as_str()).or_insert(e.span);
                    }
                }
            }
            E::Unary {
                op: ast::UnaryOp::Delete,
                expr,
            } if self.paths.is_some() => {
                if let Some(paths) = self.paths.as_deref_mut() {
                    paths.push(expr);
                }
            }
            E::Assign { target, .. } | E::Update { target, .. } => {
                if let (Some(paths), E::Member { .. } | E::Index { .. }) =
                    (self.paths.as_deref_mut(), &target.kind)
                {
                    paths.push(target);
                }
                if self.direct {
                    // Also every variable a destructuring target names (`[a, b] = [b, a]`).
                    let mut names = vec![];
                    target_names(target, &mut names);
                    for id in names {
                        self.out.entry(id.name.as_str()).or_insert(e.span);
                    }
                }
            }
            E::Arrow { params, body, .. } => {
                let mut names = vec![];
                for p in params {
                    param_names(&p.name, p.pattern.as_ref(), &mut names);
                }
                let defaults = params.iter().filter_map(|p| p.default.as_ref());
                let body = match body {
                    ast::ArrowBody::Block(b) => Body::Block(b),
                    ast::ArrowBody::Expr(x) => Body::Expr(x),
                };
                return self.closure(names, defaults.collect(), body);
            }
            E::Function(f) => return self.function(f),
            E::Object(props) if !self.fields => {
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
        let mut names = vec![];
        for p in params {
            param_names(&p.name, p.pattern.as_ref(), &mut names);
        }
        let defaults = params.iter().filter_map(|p| p.default.as_ref()).collect();
        self.closure(names, defaults, Body::Block(&f.body));
    }

    /// What a closure assigns, except its parameters and the locals its body declares (at its
    /// top level, before the assignment: an earlier one may name an enclosing variable).
    fn closure(&mut self, params: Vec<&'a str>, defaults: Vec<&'a ast::Expr>, body: Body<'a>) {
        let mut own: FxHashSet<&'a str> = params.into_iter().collect();
        let mut inner = Assigned::default();
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

    fn add_free(&mut self, names: Assigned<'a>, own: &FxHashSet<&'a str>) {
        for (name, span) in names {
            if !own.contains(name) {
                self.out.entry(name).or_insert(span);
            }
        }
    }
}

/// The names a declaration pattern binds.
/// The names a parameter binds: its own, or those of the pattern it destructures.
fn param_names<'a>(
    name: &'a ast::Ident,
    pattern: Option<&'a ast::Pattern>,
    out: &mut Vec<&'a str>,
) {
    match pattern {
        Some(p) => {
            let mut names = FxHashSet::default();
            bound_names(p, &mut names);
            out.extend(names);
        }
        None => out.push(name.name.as_str()),
    }
}

fn bound_names<'a>(p: &'a ast::Pattern, out: &mut FxHashSet<&'a str>) {
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

/// The variables assignment target `t` names, also in a destructuring target (`[a, { b }]`).
pub(crate) fn target_names<'a>(t: &'a ast::Expr, out: &mut Vec<&'a ast::Ident>) {
    use ast::ExprKind as E;
    match &t.kind {
        E::Ident(id) => out.push(id),
        E::Paren(x) | E::Spread(x) => target_names(x, out),
        E::Assign { target, .. } => target_names(target, out),
        E::Array(items) => items.iter().for_each(|x| target_names(x, out)),
        E::Object(props) => {
            for p in props {
                match p {
                    ast::ObjectProp::KeyValue(_, x) | ast::ObjectProp::Spread(x) => {
                        target_names(x, out)
                    }
                    ast::ObjectProp::Shorthand(id) => out.push(id),
                    ast::ObjectProp::Method(_) => {}
                }
            }
        }
        _ => {}
    }
}
