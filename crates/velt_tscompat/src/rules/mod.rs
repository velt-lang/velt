//! The syntax rules: one walk over each module ([`velt_syntax::visit`]) hands every node to the
//! rules that look at its kind.
//!
//! - [`types`]: `velt-number-type`, `bool-type`, `promise-error-type`, and suffixes in literal
//!   types (`number-suffix`);
//! - [`exprs`]: `number-suffix`, `int-cast`, named object literals (`struct`);
//! - [`decls`]: `struct`, `extend`, `throws`, `interface-body`, `declare-fn`;
//! - [`imports`]: `velt-import`, `outside-import`, `jsx-provider`;
//! - [`jsx_pragma`]: a `@jsxImportSource` pragma in a line comment (`jsx-pragma-comment`).

mod decls;
mod exprs;
mod imports;
mod jsx_pragma;
mod types;

use std::collections::HashSet;
use std::path::Path;

use velt_syntax::ast;
use velt_syntax::visit::{self, Visit};

pub(crate) use crate::findings::Cx;
use crate::{Finding, LintModule};

/// Every finding in `module`; `scope` is the files being linted.
pub(crate) fn lint_module(module: &LintModule, scope: &[&Path]) -> Vec<Finding> {
    let mut walk = Walk {
        cx: Cx::new(module.src),
        cast_targets: HashSet::new(),
        bodied_returns: HashSet::new(),
    };
    visit::walk_module(module.ast, &mut walk);
    let mut cx = walk.cx;
    for f in &mut cx.findings {
        if f.code == types::PROMISE_ERROR && !walk.bodied_returns.contains(&(f.span.lo, f.span.hi))
        {
            types::promise_error_without_body(f);
        }
    }
    imports::check(module, scope, &mut cx);
    jsx_pragma::check(module, &mut cx);
    cx.findings
}

/// The visitor: hands nodes to the rules.
struct Walk<'a> {
    cx: Cx<'a>,
    /// Targets of `as` casts to integer types, which `int-cast` reports as a whole (so
    /// `velt-number-type` skips them).
    cast_targets: HashSet<(u32, u32)>,
    /// Return types of functions, methods and arrows with a body, where Velt infers what a
    /// promise rejects with (so `promise-error-type` has a fix there only).
    bodied_returns: HashSet<(u32, u32)>,
}

impl Walk<'_> {
    fn bodied_return(&mut self, ret: Option<&ast::TypeExpr>) {
        if let Some(t) = ret {
            self.bodied_returns.insert((t.span.lo, t.span.hi));
        }
    }
}

impl<'a> Visit<'a> for Walk<'a> {
    fn item(&mut self, item: &'a ast::Item) {
        decls::item(item, &mut self.cx);
    }

    fn function(&mut self, sig: &'a ast::FnSig, _body: &'a ast::Block) {
        self.bodied_return(sig.ret.as_ref());
        decls::throws_clause(sig, true, &mut self.cx);
    }

    fn stmt(&mut self, s: &'a ast::Stmt) {
        // `walk_module` leaves the defaults in patterns out.
        match &s.kind {
            ast::StmtKind::ForOf { pattern, .. }
            | ast::StmtKind::Try {
                catch: Some((Some(pattern), _)),
                ..
            } => visit::walk_pattern(pattern, self),
            _ => {}
        }
    }

    fn var_decl(&mut self, v: &'a ast::VarDecl) {
        visit::walk_pattern(&v.pattern, self);
    }

    fn expr(&mut self, e: &'a ast::Expr) {
        if let Some(target) = exprs::expr(e, &mut self.cx) {
            self.cast_targets.insert((target.lo, target.hi));
        }
        if let ast::ExprKind::Arrow { params, ret, .. } = &e.kind {
            self.bodied_return(ret.as_ref());
            // `walk_module` leaves arrow parameter defaults out.
            for default in params.iter().filter_map(|p| p.default.as_ref()) {
                visit::walk_expr(default, self);
            }
        }
    }

    fn ty(&mut self, t: &'a ast::TypeExpr) {
        if !self.cast_targets.contains(&(t.span.lo, t.span.hi)) {
            types::ty(t, &mut self.cx);
        }
    }
}
