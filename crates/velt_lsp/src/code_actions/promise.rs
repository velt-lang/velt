//! Floating promises: an expression statement whose value is a `Promise<T>` (`fetchUser();`) is a
//! compile error ("floating promise": its result and errors would be lost). The fixes: `await` it
//! (inside an async function) or run it in the background with `spawn(...)`; they resolve the
//! compiler's diagnostic on that statement.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast::{self, ExprKind as E};

use super::Fix;
use crate::analysis::Analysis;
use crate::sema_query;
use velt_syntax::visit::{self, Visit};

/// Fixes for floating promises among the statements overlapping `lo..hi`.
pub fn fixes(analysis: &Analysis, lo: u32, hi: u32) -> Vec<Fix> {
    let mut scan = Scan::default();
    visit::walk_module(&analysis.module().ast, &mut scan);
    let mut out = vec![];
    for &e in &scan.statements {
        if e.span.hi < lo || e.span.lo > hi || !is_promise(analysis, e) {
            continue;
        }
        let code = analysis.snippet(e.span);
        let diagnostic = floating_diagnostic(analysis, e.span);
        if scan.in_async(e.span) {
            out.push(Fix {
                title: "Add `await`".into(),
                edits: vec![(
                    Span::new(e.span.file, e.span.lo, e.span.lo),
                    "await ".into(),
                )],
                diagnostic: diagnostic.clone(),
                preferred: true,
            });
        }
        out.push(Fix {
            title: "Run it in the background with `spawn(...)`".into(),
            edits: vec![(e.span, format!("spawn({code})"))],
            diagnostic,
            preferred: false,
        });
    }
    out
}

/// The compiler's "floating promise" error on the statement at `span`, if reported.
fn floating_diagnostic(analysis: &Analysis, span: Span) -> Option<Diagnostic> {
    analysis
        .diagnostics
        .iter()
        .find(|d| {
            d.message.starts_with("floating promise")
                && d.labels.first().is_some_and(|l| l.span == span)
        })
        .cloned()
}

fn is_promise(analysis: &Analysis, e: &ast::Expr) -> bool {
    if !matches!(e.kind, E::Call { .. }) {
        return false;
    }
    sema_query::type_at(analysis, e.span.hi).is_some_and(|t| t.starts_with("Promise<"))
}

#[derive(Default)]
struct Scan<'a> {
    /// Expressions used as statements.
    statements: Vec<&'a ast::Expr>,
    /// Function and arrow bodies: (span, is async).
    bodies: Vec<(Span, bool)>,
}

impl Scan<'_> {
    /// Whether the innermost function around `span` is async.
    fn in_async(&self, span: Span) -> bool {
        self.bodies
            .iter()
            .filter(|(b, _)| b.lo <= span.lo && span.hi <= b.hi)
            .min_by_key(|(b, _)| b.hi - b.lo)
            .is_some_and(|(_, is_async)| *is_async)
    }
}

impl<'a> Visit<'a> for Scan<'a> {
    fn function(&mut self, sig: &'a ast::FnSig, body: &'a ast::Block) {
        self.bodies.push((body.span, sig.is_async));
    }

    fn stmt(&mut self, s: &'a ast::Stmt) {
        if let ast::StmtKind::Expr(e) = &s.kind {
            self.statements.push(e);
        }
    }

    fn expr(&mut self, e: &'a ast::Expr) {
        if let E::Arrow { is_async, .. } = &e.kind {
            self.bodies.push((e.span, *is_async));
        }
    }
}
