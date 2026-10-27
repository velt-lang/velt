//! Where a function's own code last assigns each variable (`Frame::assign_ends`). A closure
//! created after that point sees the narrowing its variables have where it is created, as in
//! TypeScript; one created earlier may run after a later assignment, so it does not
//! (`FnCx::lookup_local`). An assignment inside a loop counts at the end of the outermost loop
//! around it: the next iteration runs it after a closure created further down the body.
//! Closures' own assignments are not counted: such variables are never narrowed in closures
//! (`closure_assigned`).

use rustc_hash::FxHashMap;

use velt_syntax::ast;

/// Per variable name: the source offset after which the function no longer assigns it.
pub(crate) fn assignment_ends<'a>(
    stmts: &'a [ast::Stmt],
    exprs: impl IntoIterator<Item = &'a ast::Expr>,
) -> FxHashMap<String, u32> {
    let mut w = Walk {
        out: FxHashMap::default(),
        loop_hi: None,
    };
    exprs.into_iter().for_each(|e| w.expr(e));
    stmts.iter().for_each(|s| w.stmt(s));
    w.out
}

struct Walk {
    out: FxHashMap<String, u32>,
    /// The end of the outermost loop being walked.
    loop_hi: Option<u32>,
}

impl Walk {
    fn stmt(&mut self, s: &ast::Stmt) {
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
            S::While { cond, body } | S::DoWhile { body, cond } => self.in_loop(s, |w| {
                w.expr(cond);
                w.block(body);
            }),
            S::For {
                init,
                cond,
                update,
                body,
            } => {
                init.iter().for_each(|i| self.stmt(i));
                self.in_loop(s, |w| {
                    cond.iter().chain(update).for_each(|e| w.expr(e));
                    w.block(body);
                });
            }
            S::ForOf {
                pattern,
                iter,
                body,
                ..
            } => {
                self.expr(iter);
                self.in_loop(s, |w| {
                    w.pattern(pattern);
                    w.block(body);
                });
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
            S::Return(None) | S::Break(_) | S::Continue(_) | S::Item(_) | S::Empty => {}
        }
    }

    fn in_loop(&mut self, s: &ast::Stmt, f: impl FnOnce(&mut Self)) {
        let outer = self.loop_hi;
        self.loop_hi = Some(outer.unwrap_or(s.span.hi));
        f(self);
        self.loop_hi = outer;
    }

    fn block(&mut self, b: &ast::Block) {
        b.stmts.iter().for_each(|s| self.stmt(s));
    }

    fn pattern(&mut self, p: &ast::Pattern) {
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

    fn expr(&mut self, e: &ast::Expr) {
        use ast::ExprKind as E;
        match &e.kind {
            // A closure's own assignments are `closure_assigned`'s.
            E::Arrow { .. } | E::Function(_) => return,
            E::Object(props) => {
                for p in props {
                    if let ast::ObjectProp::KeyValue(_, x) | ast::ObjectProp::Spread(x) = p {
                        self.expr(x);
                    }
                }
                return;
            }
            E::Assign { target, .. } | E::Update { target, .. } => self.target(target, e.span.lo),
            _ => {}
        }
        crate::ast_walk::children(e, &mut |c| self.expr(c));
    }

    /// The variables an assignment target names (also in a destructuring target).
    fn target(&mut self, t: &ast::Expr, at: u32) {
        use ast::ExprKind as E;
        match &t.kind {
            E::Ident(id) => self.assigned(&id.name, at),
            E::Paren(x) => self.target(x, at),
            E::Array(items) => items.iter().for_each(|x| self.target(x, at)),
            E::Spread(x) => self.target(x, at),
            E::Assign { target, .. } => self.target(target, at),
            E::Object(props) => {
                for p in props {
                    match p {
                        ast::ObjectProp::KeyValue(_, x) | ast::ObjectProp::Spread(x) => {
                            self.target(x, at)
                        }
                        ast::ObjectProp::Shorthand(id) => self.assigned(&id.name, at),
                        ast::ObjectProp::Method(_) => {}
                    }
                }
            }
            _ => {}
        }
    }

    fn assigned(&mut self, name: &str, at: u32) {
        let end = self.loop_hi.unwrap_or(at);
        let e = self.out.entry(name.to_string()).or_insert(end);
        *e = (*e).max(end);
    }
}
