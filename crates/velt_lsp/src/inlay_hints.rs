//! Inlay hints: the inferred type after `const`/`let`/`for ... of` bindings written without a type
//! (`const total: i64 = ...`), parameter names before call arguments (`area(r: 2.0)`), and on
//! declarations what inference decided but the source does not say: `throws E` after the
//! signature of a function without a `throws` clause, `modifies this` after the signature of a
//! method that modifies its receiver, and `modified` before each parameter whose contents the
//! function modifies.
//!
//! Types come from sema (`type_at` on the binding identifier), the inferred effects from
//! `throws_of` / `mutation_of`; parameter names from the callee's signature ([`callable`]).
//! Hints are left out where they would only repeat the source: bindings initialized with
//! `new T(...)` / `T { ... }` or an arrow function, and arguments that already are a name equal
//! to the parameter's.

use lsp_types::{InlayHint, InlayHintKind, InlayHintLabel};
use velt_common::Span;
use velt_syntax::ast::{self, ExprKind as E};

use crate::analysis::Analysis;
use crate::index::pattern_idents;
use crate::line_index::LineIndex;
use crate::syntax_walk::{self, Visit};
use crate::text_scan::{self, TokenKind};
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
        .map(|(at, text, kind)| {
            // Effect hints read as words of their own: `) modifies this`, `modified cart`.
            let words = !text.starts_with(':') && !text.ends_with(':');
            InlayHint {
                position: index.position(at),
                label: InlayHintLabel::String(text),
                kind: Some(kind),
                text_edits: None,
                tooltip: None,
                padding_left: (words && kind == InlayHintKind::TYPE).then_some(true),
                padding_right: (kind == InlayHintKind::PARAMETER).then_some(true),
                data: None,
            }
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
    fn function(&mut self, sig: &'a ast::FnSig, _body: &'a ast::Block) {
        if self.in_range(sig.name.span) {
            self.effects(sig);
        }
    }

    fn var_decl(&mut self, v: &'a ast::VarDecl) {
        if let Some(arrow) = v.init.as_ref().filter(|i| self.in_range(i.span)) {
            self.arrow_throws(&v.pattern, arrow);
        }
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

impl Collector<'_> {
    /// `throws E`, `modifies this` and `modified` hints for a declared function or method.
    fn effects(&mut self, sig: &ast::FnSig) {
        let Some(ide) = self.analysis.ide.as_ref() else {
            return;
        };
        let mid = (sig.name.span.lo + sig.name.span.hi) / 2;
        let Some(def) = sema_query::def_at(self.analysis, mid) else {
            return;
        };
        let after = params_end(self.analysis.text(), sig);
        if sig.throws.is_none() {
            if let Some(t) = ide.throws_of(&def) {
                let at = sig.ret.as_ref().map_or(after, |r| r.span.hi);
                self.hints
                    .push((at, format!("throws {t}"), InlayHintKind::TYPE));
            }
        }
        let Some(m) = ide.mutation_of(&def) else {
            return;
        };
        if m.this {
            // After the whole signature, written `throws` included, and after an inferred
            // `throws` hint at the same place: `add(x: T): R throws E modifies this`.
            let end = sig.throws.as_ref().or(sig.ret.as_ref());
            let at = end.map_or(after, |t| t.span.hi);
            let hint = (at, "modifies this".to_string(), InlayHintKind::TYPE);
            self.hints.push(hint);
        }
        for p in sig
            .params
            .iter()
            .filter(|p| m.params.contains(&p.name.name))
        {
            let hint = (
                p.name.span.lo,
                "modified".to_string(),
                InlayHintKind::PARAMETER,
            );
            self.hints.push(hint);
        }
    }

    /// `throws E` before the `=>` of an arrow function that initializes a variable.
    fn arrow_throws(&mut self, pattern: &ast::Pattern, init: &ast::Expr) {
        let E::Arrow {
            throws: None,
            ret,
            body,
            ..
        } = &init.kind
        else {
            return;
        };
        let (Some(ide), Some(name)) = (
            self.analysis.ide.as_ref(),
            pattern_idents(pattern).first().copied(),
        ) else {
            return;
        };
        let Some(def) = sema_query::def_at(self.analysis, (name.span.lo + name.span.hi) / 2) else {
            return;
        };
        let Some(t) = ide.throws_of(&def) else {
            return;
        };
        let body_lo = match body {
            ast::ArrowBody::Expr(e) => e.span.lo,
            ast::ArrowBody::Block(b) => b.span.lo,
        };
        let Some(head) = self
            .analysis
            .text()
            .get(init.span.lo as usize..body_lo as usize)
        else {
            return;
        };
        // After the last `)` of the head as the scanner sees it (not one inside a comment). A
        // bare parameter (`x => …`) needs a written function type, which says what it throws.
        let close = text_scan::scan(head, head.len())
            .into_iter()
            .rev()
            .find(|t| t.kind == TokenKind::Punct(b')'));
        let Some(end) = close.map(|c| c.hi) else {
            return;
        };
        let at = ret.as_ref().map_or(init.span.lo + end, |r| r.span.hi);
        self.hints
            .push((at, format!("throws {t}"), InlayHintKind::TYPE));
    }
}

/// The offset just after the `)` that closes the parameter list of `sig`: the first `(` after
/// the name outside the type parameters (whose bounds may hold function types) and its
/// matching `)`, comments and strings skipped.
fn params_end(text: &str, sig: &ast::FnSig) -> u32 {
    let from = sig.name.span.hi;
    let fallback = sig.params.last().map_or(from, |p| p.span.hi);
    // Only the signature is scanned (offsets relative to `from`).
    let Some(signature) = text.get(from as usize..sig.span.hi.max(fallback) as usize) else {
        return fallback;
    };
    let (mut angles, mut parens) = (0usize, 0usize);
    let mut prev: Option<text_scan::Token> = None;
    for t in text_scan::scan(signature, signature.len()) {
        let arrow = prev.is_some_and(|p| p.kind == TokenKind::Punct(b'=') && p.hi == t.lo);
        prev = Some(t);
        match t.kind {
            TokenKind::Punct(b'<') if parens == 0 => angles += 1,
            TokenKind::Punct(b'>') if parens == 0 && !arrow => angles = angles.saturating_sub(1),
            TokenKind::Punct(b'(') if angles == 0 => parens += 1,
            TokenKind::Punct(b')') if angles == 0 && parens == 1 => return from + t.hi,
            TokenKind::Punct(b')') if angles == 0 => parens = parens.saturating_sub(1),
            TokenKind::Punct(b'{') if angles == 0 && parens == 0 => break,
            _ => {}
        }
    }
    fallback
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
