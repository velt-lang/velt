//! Document symbols (outline): functions, classes/structs with their members, interfaces, enums
//! with variants, type aliases, globals and `extend` blocks.

use lsp_types::{DocumentSymbol, SymbolKind};
use velt_common::Span;
use velt_syntax::ast;

use crate::analysis::Analysis;
use crate::index::pattern_idents;
use crate::line_index::LineIndex;
use crate::signature;

/// The outline of the document.
pub fn document_symbols(analysis: &Analysis) -> Vec<DocumentSymbol> {
    let b = Builder {
        analysis,
        index: LineIndex::new(analysis.text()),
    };
    analysis
        .module()
        .ast
        .items
        .iter()
        .flat_map(|item| b.item(item))
        .collect()
}

struct Builder<'a> {
    analysis: &'a Analysis,
    index: LineIndex<'a>,
}

impl Builder<'_> {
    fn item(&self, item: &ast::Item) -> Vec<DocumentSymbol> {
        let a = self.analysis;
        let one = |name: &ast::Ident, kind, children| {
            let detail = signature::item(a, item);
            vec![self.symbol(name, kind, item.span, Some(detail), children)]
        };
        match &item.kind {
            ast::ItemKind::Function(f) => one(&f.sig.name, SymbolKind::FUNCTION, vec![]),
            ast::ItemKind::ExternFn(sig) => one(&sig.name, SymbolKind::FUNCTION, vec![]),
            ast::ItemKind::Class(t) => one(&t.name, SymbolKind::CLASS, self.type_members(t)),
            ast::ItemKind::Struct(t) => one(&t.name, SymbolKind::STRUCT, self.type_members(t)),
            ast::ItemKind::Interface(i) => {
                let mut children = self.fields(&i.fields);
                children.extend(i.methods.iter().map(|m| self.method(&m.sig, m.sig.span)));
                one(&i.name, SymbolKind::INTERFACE, children)
            }
            ast::ItemKind::Enum(e) => {
                let children = e
                    .variants
                    .iter()
                    .map(|v| self.symbol(&v.name, SymbolKind::ENUM_MEMBER, v.span, None, vec![]))
                    .collect();
                one(&e.name, SymbolKind::ENUM, children)
            }
            ast::ItemKind::TypeAlias(t) => one(&t.name, SymbolKind::TYPE_PARAMETER, vec![]),
            ast::ItemKind::Var(v) => self.globals(item, v),
            ast::ItemKind::Extend(ext) => self.extend(item, ext),
            ast::ItemKind::Import(_) => vec![],
        }
    }

    fn type_members(&self, t: &ast::TypeDecl) -> Vec<DocumentSymbol> {
        let mut out = self.fields(&t.fields);
        if let Some(ctor) = &t.constructor {
            let span = ctor.sig.span.to(ctor.body.span);
            out.push(self.symbol(&ctor.sig.name, SymbolKind::CONSTRUCTOR, span, None, vec![]));
        }
        for m in &t.methods {
            out.push(self.method(&m.decl.sig, m.decl.sig.span.to(m.decl.body.span)));
        }
        out
    }

    fn fields(&self, fields: &[ast::Field]) -> Vec<DocumentSymbol> {
        fields
            .iter()
            .map(|f| {
                let detail = self.analysis.snippet(f.ty.span).to_string();
                self.symbol(&f.name, SymbolKind::FIELD, f.span, Some(detail), vec![])
            })
            .collect()
    }

    fn method(&self, sig: &ast::FnSig, span: Span) -> DocumentSymbol {
        let detail = signature::function(self.analysis, sig);
        self.symbol(&sig.name, SymbolKind::METHOD, span, Some(detail), vec![])
    }

    fn globals(&self, item: &ast::Item, v: &ast::VarDecl) -> Vec<DocumentSymbol> {
        let kind = match v.kind {
            ast::VarKind::Const => SymbolKind::CONSTANT,
            ast::VarKind::Let | ast::VarKind::Using | ast::VarKind::AwaitUsing => {
                SymbolKind::VARIABLE
            }
        };
        pattern_idents(&v.pattern)
            .into_iter()
            .map(|name| self.symbol(name, kind, item.span, None, vec![]))
            .collect()
    }

    fn extend(&self, item: &ast::Item, ext: &ast::ExtendDecl) -> Vec<DocumentSymbol> {
        let name = ast::Ident {
            name: format!("extend {}", self.analysis.snippet(ext.target.span)),
            span: ext.target.span,
        };
        let children = ext
            .methods
            .iter()
            .map(|m| self.method(&m.decl.sig, m.decl.sig.span.to(m.decl.body.span)))
            .collect();
        vec![self.symbol(&name, SymbolKind::NAMESPACE, item.span, None, children)]
    }

    fn symbol(
        &self,
        name: &ast::Ident,
        kind: SymbolKind,
        span: Span,
        detail: Option<String>,
        children: Vec<DocumentSymbol>,
    ) -> DocumentSymbol {
        // Clients require the selection (the name) to lie inside the full range.
        let full = span.to(name.span);
        #[allow(deprecated)]
        // `deprecated` is a required field of the LSP struct, superseded by `tags`
        DocumentSymbol {
            // Clients reject empty names, which parser recovery can produce.
            name: if name.name.is_empty() {
                "<missing name>".into()
            } else {
                name.name.clone()
            },
            detail,
            kind,
            tags: None,
            deprecated: None,
            range: self.index.range(full.lo, full.hi),
            selection_range: self.index.range(name.span.lo, name.span.hi),
            children: (!children.is_empty()).then_some(children),
        }
    }
}
