//! Completion inside JSX. The context is read from the text before the cursor (while typing, the
//! element usually does not parse): right after `<` the tags (the intrinsic elements of the
//! file's JSX runtime, then the components in scope); inside an opening tag after its name the
//! attributes of that tag (intrinsic: the fields of `JSX.IntrinsicElements[tag]`; component: the
//! fields of its props parameter), minus the ones already written. After `</` the innermost open
//! element comes first. Answers come from sema's IDE queries ([`velt_sema::ide`]); where the
//! cursor is comes from [`context`].

use lsp_types::{CompletionItem, CompletionItemKind, CompletionTextEdit, TextEdit};
use velt_sema::ide::{DefKind, DefRef};
use velt_syntax::ast;

use crate::analysis::Analysis;
use crate::index::pattern_idents;
use crate::index::scope::is_component;
use crate::line_index::LineIndex;

use crate::sema_query::{item, kind};

mod context;

pub use context::{context, word_start, Context};

/// Completion items for `ctx` at byte `offset` of the document. `replace`: the byte range the
/// chosen item replaces, when the word holds a `-` an editor may not count (`aria-l`).
pub fn items(
    analysis: &Analysis,
    ctx: &Context<'_>,
    offset: u32,
    replace: Option<(u32, u32)>,
) -> Vec<CompletionItem> {
    let Some(ide) = analysis.ide.as_ref() else {
        return vec![];
    };
    let file = analysis.file();
    let mut out = match ctx {
        Context::Tag { closing } => {
            let tags = ide.jsx_intrinsics(file).iter();
            let mut out: Vec<CompletionItem> = tags
                .map(|(tag, _, ty)| item(tag, CompletionItemKind::PROPERTY, ty))
                .collect();
            let components = ide.scope_at(file, offset).into_iter().filter(|(name, d)| {
                is_component(name) && (d.kind == DefKind::Function || has_function_type(d))
            });
            out.extend(components.map(|(name, d)| item(&name, kind(d.kind), &d.detail)));
            if let Some(open) = closing {
                closing_first(&mut out, open);
            }
            out
        }
        Context::Attribute { tag, written } => {
            let attrs = if is_component(tag) {
                props_of(analysis, tag, offset)
            } else {
                let tags = ide.jsx_intrinsics(file);
                let found = tags.iter().find(|(name, _, _)| name == tag);
                found.map_or_else(Vec::new, |(_, def, _)| ide.members_of(def))
            };
            attrs
                .into_iter()
                .filter(|(name, _, _)| !written.contains(&name.as_str()))
                .map(|(name, _, ty)| item(&name, CompletionItemKind::FIELD, &ty))
                .collect()
        }
    };
    if let Some((lo, hi)) = replace {
        let range = LineIndex::new(analysis.text()).range(lo, hi);
        for i in &mut out {
            i.text_edit = Some(CompletionTextEdit::Edit(TextEdit::new(
                range,
                i.label.clone(),
            )));
        }
    }
    out
}

/// After `</`: the innermost open element first, preselected (it is what closes here); the
/// others keep their order behind it.
fn closing_first(items: &mut Vec<CompletionItem>, open: &str) {
    for (i, item) in items.iter_mut().enumerate() {
        item.sort_text = Some(format!("1{i:05}"));
    }
    match items.iter_mut().find(|i| i.label == open) {
        Some(found) => {
            found.sort_text = Some("0".into());
            found.preselect = Some(true);
        }
        None => items.insert(
            0,
            CompletionItem {
                sort_text: Some("0".into()),
                preselect: Some(true),
                ..item(open, CompletionItemKind::PROPERTY, "")
            },
        ),
    }
    items.sort_by(|a, b| a.sort_text.cmp(&b.sort_text));
}

/// Is a variable's type a function type (`const Card: (props: P) => Element`)? Its detail
/// spells the type after the name.
fn has_function_type(d: &DefRef) -> bool {
    matches!(
        d.kind,
        DefKind::Local | DefKind::Constant | DefKind::Parameter
    ) && d
        .detail
        .split_once(": ")
        .is_some_and(|(_, ty)| ty.starts_with('('))
}

/// The fields of the props of component `tag` (a function or an arrow-valued constant in scope
/// at `offset`, or `ns.Name` of a namespace import): the members of its first parameter.
fn props_of(analysis: &Analysis, tag: &str, offset: u32) -> Vec<(String, DefRef, String)> {
    let Some(ide) = analysis.ide.as_ref() else {
        return vec![];
    };
    let file = analysis.file();
    let found = match tag.split_once('.') {
        // `<ui.Card`: a namespace import's export.
        Some((ns, name)) => ide
            .namespace_members(file, ns)
            .into_iter()
            .find(|(n, _)| n == name),
        None => ide
            .scope_at(file, offset)
            .into_iter()
            .find(|(n, _)| n == tag),
    };
    let Some((_, component)) = found else {
        return vec![];
    };
    let Some((param, body_lo)) = analysis
        .modules
        .get(component.module)
        .and_then(|m| first_param(&m.ast, &component))
    else {
        return vec![];
    };
    let inside = ide.scope_at(component.span.file, body_lo + 1);
    inside
        .into_iter()
        .find(|(name, d)| *name == param && d.kind == DefKind::Parameter)
        .map_or_else(Vec::new, |(_, p)| ide.members_of(&p))
}

/// The first parameter's name of the function or arrow constant `def` declares, and where its
/// body starts.
fn first_param(module: &ast::Module, def: &DefRef) -> Option<(String, u32)> {
    module.items.iter().find_map(|it| match &it.kind {
        ast::ItemKind::Function(f) if f.sig.name.span == def.span => {
            Some((f.sig.params.first()?.name.name.clone(), f.body.span.lo))
        }
        ast::ItemKind::Var(v)
            if pattern_idents(&v.pattern)
                .iter()
                .any(|i| i.span == def.span) =>
        {
            let ast::ExprKind::Arrow { params, body, .. } = &v.init.as_ref()?.kind else {
                return None;
            };
            let body_lo = match body {
                ast::ArrowBody::Expr(e) => e.span.lo,
                ast::ArrowBody::Block(b) => b.span.lo,
            };
            Some((params.first()?.name.name.clone(), body_lo))
        }
        _ => None,
    })
}
