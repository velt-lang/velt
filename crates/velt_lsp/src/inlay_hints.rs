//! Inlay hints: the inferred type after `const`/`let`/`for ... of` bindings written without a type
//! (`const total: i64 = ...`), and parameter names before call arguments (`area(r: 2.0)`).
//!
//! Types come from sema (`type_at` on the binding identifier); parameter names from the callee's
//! signature ([`callable`]). Hints are left out where they would only repeat the source: bindings
//! initialized with `new T(...)` / `T { ... }` or an arrow function, and arguments that already are
//! a name equal to the parameter's.

use lsp_types::{InlayHint, InlayHintKind, InlayHintLabel};
use velt_common::Span;
use velt_syntax::ast::{self, ExprKind as E};

use crate::analysis::Analysis;
use crate::index::pattern_idents;
use crate::line_index::LineIndex;
use crate::syntax_walk::{self, Visit};
use crate::{callable, sema_query};

/// Hints for the document's byte range `lo..hi`.
pub fn inlay_hints(analysis: &Analysis, lo: u32, hi: u32) -> Vec<InlayHint> {
    let mut c = Collector {
        analysis,
        lo,
        hi,
        hints: vec![],
    };
    syntax_walk::walk_module(&analysis.module().ast, &mut c);
    let index = LineIndex::new(analysis.text());
    c.hints
        .into_iter()
        .map(|(at, text, kind)| InlayHint {
            position: index.position(at),
            label: InlayHintLabel::String(text),
            kind: Some(kind),
            text_edits: None,
            tooltip: None,
            padding_left: None,
            padding_right: (kind == InlayHintKind::PARAMETER).then_some(true),
            data: None,
        })
        .collect()
}

struct Collector<'a> {
    analysis: &'a Analysis,
    lo: u32,
    hi: u32,
    /// (offset, label, kind)
    hints: Vec<(u32, String, InlayHintKind)>,
}

impl Collector<'_> {
    fn in_range(&self, span: Span) -> bool {
        span.file == self.analysis.file() && span.hi >= self.lo && span.lo <= self.hi
    }

    fn binding_types(&mut self, pattern: &ast::Pattern) {
        for ident in pattern_idents(pattern) {
            if !self.in_range(ident.span) {
                continue;
            }
            let mid = (ident.span.lo + ident.span.hi) / 2;
            if let Some(ty) = sema_query::type_at(self.analysis, mid) {
                if ty != "{error}" && !ty.is_empty() {
                    let hint = (ident.span.hi, format!(": {ty}"), InlayHintKind::TYPE);
                    self.hints.push(hint);
                }
            }
        }
    }

    fn argument_names(&mut self, callee: Option<&ast::Ident>, is_new: bool, args: &[ast::Expr]) {
        let Some(callee) = callee else {
            return;
        };
        let mid = (callee.span.lo + callee.span.hi) / 2;
        let Some(def) = sema_query::def_at(self.analysis, mid) else {
            return;
        };
        let Some(sig) = callable::signature_of(self.analysis, &def, is_new) else {
            return;
        };
        for (arg, param) in args.iter().zip(&sig.params) {
            if matches!(arg.kind, E::Spread(_)) {
                break;
            }
            if !self.in_range(arg.span) || !worth_naming(&param.name, arg) {
                continue;
            }
            let hint = (
                arg.span.lo,
                format!("{}:", param.name),
                InlayHintKind::PARAMETER,
            );
            self.hints.push(hint);
        }
    }
}

impl<'a> Visit<'a> for Collector<'_> {
    fn var_decl(&mut self, v: &'a ast::VarDecl) {
        let obvious = v.init.as_ref().is_some_and(|init| {
            matches!(
                init.kind,
                E::New { .. } | E::StructLit { .. } | E::Arrow { .. }
            )
        });
        if v.ty.is_none() && !obvious {
            self.binding_types(&v.pattern);
        }
    }

    fn stmt(&mut self, s: &'a ast::Stmt) {
        if let ast::StmtKind::ForOf { pattern, .. } = &s.kind {
            self.binding_types(pattern);
        }
    }

    fn expr(&mut self, e: &'a ast::Expr) {
        if !self.in_range(e.span) {
            return;
        }
        match &e.kind {
            E::Call { callee, args, .. } => {
                let name = match &callee.kind {
                    E::Ident(i) => Some(i),
                    E::Member { prop, .. } => Some(prop),
                    _ => None,
                };
                self.argument_names(name, false, args);
            }
            E::New { class, args } => {
                let name = match &class.kind {
                    ast::TypeExprKind::Named { path, .. } => path.last(),
                    _ => None,
                };
                self.argument_names(name, true, args);
            }
            _ => {}
        }
    }
}

/// Whether a parameter-name hint adds information for `arg`: the parameter has a real name and
/// the argument is not already spelled like it (`x` or `p.x` for parameter `x`).
fn worth_naming(param: &str, arg: &ast::Expr) -> bool {
    let synthetic = param
        .strip_prefix("arg")
        .is_some_and(|n| n.chars().all(|c| c.is_ascii_digit()));
    if param.is_empty() || synthetic {
        return false;
    }
    let spelled = match &arg.kind {
        E::Ident(i) => Some(&i.name),
        E::Member { prop, .. } => Some(&prop.name),
        _ => None,
    };
    spelled.is_none_or(|name| name != param)
}
