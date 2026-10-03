//! `"total: " + n + "ms"` → `` `total: ${n}ms` ``: the fix for sema's "mismatched types" error on a
//! `+` between a string and another value (its note suggests a template literal).
//!
//! The whole `+` chain around the reported operand is rewritten. Operands before the first string
//! keep their numeric meaning: `1 + n + "a"` becomes `` `${1 + n}a` ``, as the `+` would have
//! computed the sum first.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast::{self, BinaryOp, ExprKind as E};

use crate::analysis::Analysis;
use crate::sema_query;
use velt_syntax::visit::{self, Visit};

/// `(title, edits)` converting the `+` chain that `d` (primary label `span`) reports on.
pub fn for_diagnostic(
    analysis: &Analysis,
    d: &Diagnostic,
    span: Span,
) -> Option<(String, Vec<(Span, String)>)> {
    let suggests_template = d.notes.iter().any(|n| n.contains("template literal"));
    if d.message != "mismatched types" || !suggests_template {
        return None;
    }
    let mut adds = Adds(vec![]);
    visit::walk_module(&analysis.module().ast, &mut adds);
    let mut chain = adds
        .0
        .iter()
        .find(|(_, lhs, rhs)| lhs.span == span || rhs.span == span)?
        .0;
    while let Some((outer, _, _)) = adds.0.iter().find(|(_, lhs, _)| lhs.span == chain.span) {
        chain = outer;
    }
    let operands = flatten(chain);
    let first_string = operands.iter().position(|e| is_string(analysis, e))?;
    let mut out = String::from("`");
    if first_string > 1 {
        let numeric = Span::new(
            span.file,
            operands[0].span.lo,
            operands[first_string - 1].span.hi,
        );
        push_substitution(&mut out, analysis.snippet(numeric));
    } else if first_string == 1 {
        push_operand(analysis, &mut out, operands[0]);
    }
    for e in &operands[first_string..] {
        push_operand(analysis, &mut out, e);
    }
    out.push('`');
    Some((
        "Convert to a template literal".into(),
        vec![(chain.span, out)],
    ))
}

/// Every `lhs + rhs` of the document.
struct Adds<'a>(Vec<(&'a ast::Expr, &'a ast::Expr, &'a ast::Expr)>);

impl<'a> Visit<'a> for Adds<'a> {
    fn expr(&mut self, e: &'a ast::Expr) {
        if let E::Binary {
            op: BinaryOp::Add,
            lhs,
            rhs,
        } = &e.kind
        {
            self.0.push((e, lhs, rhs));
        }
    }
}

/// Operands of a left-associated `a + b + c` chain, in order.
fn flatten(e: &ast::Expr) -> Vec<&ast::Expr> {
    match &e.kind {
        E::Binary {
            op: BinaryOp::Add,
            lhs,
            rhs,
        } => {
            let mut out = flatten(lhs);
            out.push(rhs);
            out
        }
        _ => vec![e],
    }
}

fn is_string(analysis: &Analysis, e: &ast::Expr) -> bool {
    match &e.kind {
        E::Lit(ast::Lit::Str(_)) | E::Template { .. } => true,
        _ => sema_query::type_at(analysis, e.span.hi).is_some_and(|t| t == "string"),
    }
}

fn push_operand(analysis: &Analysis, out: &mut String, e: &ast::Expr) {
    match &e.kind {
        E::Lit(ast::Lit::Str(s)) => push_text(out, s),
        E::Template { quasis, exprs } => {
            for (i, q) in quasis.iter().enumerate() {
                push_text(out, q);
                if let Some(x) = exprs.get(i) {
                    push_substitution(out, analysis.snippet(x.span));
                }
            }
        }
        _ => push_substitution(out, analysis.snippet(e.span)),
    }
}

fn push_substitution(out: &mut String, code: &str) {
    out.push_str("${");
    out.push_str(code);
    out.push('}');
}

/// Literal text, escaped for a template literal.
fn push_text(out: &mut String, s: &str) {
    for c in s.chars() {
        match c {
            '`' => out.push_str("\\`"),
            '\\' => out.push_str("\\\\"),
            '$' => out.push_str("\\$"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
}
