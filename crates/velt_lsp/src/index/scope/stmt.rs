//! Statements, blocks and pattern bindings for the scope walker.

use velt_syntax::ast;

use super::{Reference, Walker};
use crate::index::{pattern_idents, type_name};
use crate::signature;

impl<'a> Walker<'a> {
    /// Walk a block containing the cursor in a new scope.
    pub(super) fn block(&mut self, block: &'a ast::Block) {
        if !self.contains(block.span) {
            return;
        }
        self.scoped(|w| {
            w.stmts(&block.stmts);
            w.snapshot();
        });
    }

    /// Statements of a block containing the cursor (in the current scope).
    pub(super) fn stmts(&mut self, stmts: &'a [ast::Stmt]) {
        for s in stmts {
            if s.span.lo > self.offset {
                self.snapshot();
                break;
            }
            if self.contains(s.span) {
                self.stmt(s);
            }
            self.declare(s);
        }
    }

    /// Bring the bindings a statement introduces into scope (without entering it).
    fn declare(&mut self, s: &'a ast::Stmt) {
        match &s.kind {
            ast::StmtKind::Var(v) => self.var_bindings(v),
            ast::StmtKind::Item(item) => {
                for ident in crate::index::item_names(item) {
                    let detail = signature::item(self.analysis, item);
                    self.bind(ident, detail, None);
                }
            }
            _ => {}
        }
    }

    fn var_bindings(&mut self, v: &'a ast::VarDecl) {
        let keyword = v.kind.keyword();
        let declared = v.ty.as_ref().and_then(type_name).map(String::from);
        let inferred = v.init.as_ref().and_then(constructed_type).map(String::from);
        let simple = matches!(v.pattern.kind, ast::PatternKind::Ident(_));
        for ident in pattern_idents(&v.pattern) {
            let detail = match (&v.ty, simple) {
                (Some(ty), true) => {
                    format!(
                        "{keyword} {}: {}",
                        ident.name,
                        self.analysis.snippet(ty.span)
                    )
                }
                _ => format!("{keyword} {}", ident.name),
            };
            let type_name = if simple {
                declared.clone().or_else(|| inferred.clone())
            } else {
                None
            };
            self.bind(ident, detail, type_name);
        }
    }

    fn stmt(&mut self, s: &'a ast::Stmt) {
        match &s.kind {
            ast::StmtKind::Var(v) => {
                self.opt_ty(v.ty.as_ref());
                self.opt_expr(v.init.as_ref());
            }
            ast::StmtKind::Expr(e) | ast::StmtKind::Throw(e) => self.expr(e),
            ast::StmtKind::Return(e) => self.opt_expr(e.as_ref()),
            ast::StmtKind::If { cond, then, els } => {
                self.expr(cond);
                self.block(then);
                if let Some(els) = els.as_deref().filter(|e| self.contains(e.span)) {
                    self.stmt(els);
                }
            }
            ast::StmtKind::While { cond, body } | ast::StmtKind::DoWhile { body, cond } => {
                self.expr(cond);
                self.block(body);
            }
            ast::StmtKind::For {
                init,
                cond,
                update,
                body,
            } => self.for_loop(init.as_deref(), cond.as_ref(), update.as_ref(), body),
            ast::StmtKind::ForOf {
                kind,
                pattern,
                iter,
                body,
            } => {
                self.expr(iter);
                let keyword = kind.keyword();
                self.scoped(|w| {
                    w.bind_pattern(pattern, keyword);
                    w.block(body);
                });
            }
            ast::StmtKind::Block(b) => self.block(b),
            ast::StmtKind::Switch {
                discriminant,
                cases,
            } => {
                self.expr(discriminant);
                for c in cases {
                    if !self.contains(c.span) {
                        continue;
                    }
                    self.opt_expr(c.test.as_ref());
                    self.scoped(|w| {
                        w.stmts(&c.body);
                        w.snapshot();
                    });
                }
            }
            ast::StmtKind::Labeled { body, .. } => self.stmt(body),
            ast::StmtKind::Try {
                body,
                catch,
                finally,
            } => {
                self.block(body);
                if let Some((pattern, handler)) = catch {
                    self.scoped(|w| {
                        if let Some(p) = pattern {
                            w.bind_pattern(p, "(catch)");
                        }
                        w.block(handler);
                    });
                }
                if let Some(f) = finally {
                    self.block(f);
                }
            }
            ast::StmtKind::Item(item) => self.item(item),
            ast::StmtKind::Break(_) | ast::StmtKind::Continue(_) | ast::StmtKind::Empty => {}
        }
    }

    fn for_loop(
        &mut self,
        init: Option<&'a ast::Stmt>,
        cond: Option<&'a ast::Expr>,
        update: Option<&'a ast::Expr>,
        body: &'a ast::Block,
    ) {
        self.scoped(|w| {
            if let Some(init) = init {
                if w.contains(init.span) {
                    w.stmt(init);
                }
                w.declare(init);
            }
            w.opt_expr(cond);
            w.opt_expr(update);
            w.block(body);
        });
    }

    /// Bind every identifier of `p` (described as `<keyword> name`).
    pub(super) fn bind_pattern(&mut self, p: &'a ast::Pattern, keyword: &str) {
        for ident in pattern_idents(p) {
            self.bind(ident, format!("{keyword} {}", ident.name), None);
        }
    }

    /// `Reference::Name` for an identifier used as a value.
    pub(super) fn value_name(&mut self, ident: &'a ast::Ident) {
        if self.contains(ident.span) {
            let local = self.lookup(&ident.name);
            self.hit(Reference::Name { ident, local });
        }
    }
}

/// The type an initializer obviously constructs: `new User(...)`, `User { ... }`.
fn constructed_type(init: &ast::Expr) -> Option<&str> {
    match &init.kind {
        ast::ExprKind::New { class, .. } => type_name(class),
        ast::ExprKind::StructLit { name, .. } => type_name(name),
        _ => None,
    }
}
