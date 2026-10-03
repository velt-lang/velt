//! Statements and blocks. Bodies of `if`/`while`/`for`/`do` always get braces (the parser already
//! wraps a braceless body in a one-statement block, so the AST is unchanged).

use velt_syntax::ast::{Block, Expr, Pattern, Stmt, StmtKind};

use super::decls::braced;
use super::Printer;
use crate::doc::{cat, group, indent, nil, softline, Doc};

impl<'a> Printer<'a> {
    /// `{ stmts }`, or `{}` when empty.
    pub(super) fn block(&mut self, block: &Block) -> Doc {
        let body = self.lines(
            &block.stmts,
            block.span.hi,
            |s| (s.span.lo, s.span.hi),
            |_, _| false,
            |p, s| p.stmt(s),
        );
        braced(body)
    }

    pub(super) fn stmt(&mut self, stmt: &Stmt) -> Doc {
        match &stmt.kind {
            StmtKind::Var(var) => cat![self.var_decl(var), ";"],
            StmtKind::Expr(e) => cat![self.expr(e), ";"],
            StmtKind::Return(value) => self.keyword_value("return", value.as_ref()),
            StmtKind::Throw(value) => self.keyword_value("throw", Some(value)),
            StmtKind::If { cond, then, els } => self.if_stmt(cond, then, els.as_deref()),
            StmtKind::While { cond, body } => {
                cat![self.paren_head("while", cond), " ", self.block(body)]
            }
            StmtKind::DoWhile { body, cond } => {
                let body = self.block(body);
                cat!["do ", body, " ", self.paren_head("while", cond), ";"]
            }
            StmtKind::For {
                init,
                cond,
                update,
                body,
            } => {
                let init = init
                    .as_deref()
                    .map(std::slice::from_ref)
                    .unwrap_or_default();
                self.for_stmt(None, init, cond.as_ref(), update.as_ref(), body)
            }
            StmtKind::ForOf {
                kind,
                pattern,
                iter,
                body,
                is_await,
            } => self.for_of(*kind, pattern, iter, body, *is_await),
            StmtKind::Break(label) => jump("break", label.as_ref().map(|l| l.name.as_str())),
            StmtKind::Continue(label) => jump("continue", label.as_ref().map(|l| l.name.as_str())),
            StmtKind::Block(b) => match self.desugared_for(b) {
                Some(doc) => doc,
                None => self.block(b),
            },
            StmtKind::Switch {
                discriminant,
                cases,
            } => self.switch_stmt(discriminant, cases, stmt.span.hi),
            StmtKind::Labeled { label, body } => {
                cat![label.name.clone(), ": ", self.stmt(body)]
            }
            StmtKind::Try {
                body,
                catch,
                finally,
            } => self.try_stmt(body, catch.as_ref(), finally.as_ref()),
            StmtKind::Item(item) => self.item(item),
            StmtKind::Empty => ";".into(),
        }
    }

    /// `return value;` / `throw value;`
    fn keyword_value(&mut self, keyword: &str, value: Option<&Expr>) -> Doc {
        match value {
            Some(v) => cat![keyword, " ", self.expr_jsx_parens(v), ";"],
            None => cat![keyword, ";"],
        }
    }

    /// `keyword (cond)`, breaking inside the parentheses when the condition is long.
    pub(super) fn paren_head(&mut self, keyword: &str, cond: &Expr) -> Doc {
        let cond = self.expr_no_indent(cond);
        group(cat![
            keyword,
            " (",
            indent(cat![softline(), cond]),
            softline(),
            ")"
        ])
    }

    fn if_stmt(&mut self, cond: &Expr, then: &Block, els: Option<&Stmt>) -> Doc {
        let head = self.paren_head("if", cond);
        let then = self.block(then);
        let els = match els {
            None => nil(),
            Some(s) => match &s.kind {
                StmtKind::Block(b) => cat![" else ", self.block(b)],
                _ => cat![" else ", self.stmt(s)],
            },
        };
        cat![head, " ", then, els]
    }

    fn try_stmt(
        &mut self,
        body: &Block,
        catch: Option<&(Option<Pattern>, Block)>,
        finally: Option<&Block>,
    ) -> Doc {
        let mut doc = cat!["try ", self.block(body)];
        if let Some((binding, handler)) = catch {
            let binding = match binding {
                Some(p) => cat!["(", self.pattern(p), ") "],
                None => nil(),
            };
            doc = cat![doc, " catch ", binding, self.block(handler)];
        }
        if let Some(f) = finally {
            doc = cat![doc, " finally ", self.block(f)];
        }
        doc
    }
}

/// `break;` / `continue label;`
fn jump(keyword: &str, label: Option<&str>) -> Doc {
    match label {
        Some(l) => cat![keyword, " ", l.to_string(), ";"],
        None => cat![keyword, ";"],
    }
}
