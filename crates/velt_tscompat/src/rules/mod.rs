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

use velt_common::Span;
use velt_syntax::ast;
use velt_syntax::visit::{self, Visit};

use crate::{Finding, Fix, LintModule, Severity};

/// Every finding in `module`; `scope` is the files being linted.
pub(crate) fn lint_module(module: &LintModule, scope: &[&Path]) -> Vec<Finding> {
    let mut walk = Walk {
        cx: Cx {
            src: module.src,
            findings: vec![],
        },
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

/// What the rules share: the source text and the findings so far.
pub(crate) struct Cx<'a> {
    src: &'a str,
    findings: Vec<Finding>,
}

impl Cx<'_> {
    /// The source text of `span` (empty if it is out of range, which a parsed module's spans
    /// never are).
    fn text(&self, span: Span) -> &str {
        self.src
            .get(span.lo as usize..span.hi as usize)
            .unwrap_or("")
    }

    /// Report an error (every syntax rule is one: `tsc` rejects the code or JavaScript runs it
    /// differently).
    fn error(&mut self, code: &'static str, span: Span, message: String, notes: &[&str]) {
        debug_assert!(
            crate::RULES.contains(&code),
            "ICE: `{code}` is not in RULES"
        );
        self.findings.push(Finding {
            code,
            severity: Severity::Error,
            span,
            message,
            notes: notes.iter().map(|n| n.to_string()).collect(),
            fix: None,
        });
    }

    /// [`Cx::error`] with a fix.
    fn error_with_fix(
        &mut self,
        code: &'static str,
        span: Span,
        message: String,
        notes: &[&str],
        fix: Fix,
    ) {
        self.error(code, span, message, notes);
        if let Some(f) = self.findings.last_mut() {
            f.fix = Some(fix);
        }
    }

    /// The span of keyword `word` inside `within` (the first whole-word occurrence; `within`
    /// itself when the source doesn't have it there).
    fn keyword(&self, within: Span, word: &str) -> Span {
        let text = self.text(within);
        let mut from = 0;
        while let Some(at) = text[from..].find(word).map(|i| i + from) {
            let before = text[..at].chars().next_back();
            let after = text[at + word.len()..].chars().next();
            let is_word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
            if !is_word(before) && !is_word(after) {
                let lo = within.lo + at as u32;
                return Span::new(within.file, lo, lo + word.len() as u32);
            }
            from = at + word.len();
        }
        within
    }
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
