//! Editor answers from sema's IDE queries (`velt_sema::ide`, docs/internals/contracts/sema_ide.md): what the
//! name under the cursor denotes, types, visible names and members. Request handlers try these
//! first and fall back to the AST [`index`](crate::index) when sema has no answer (e.g. the cursor
//! is in code the parser could not recover).

use lsp_types::{CompletionItem, CompletionItemKind};
use velt_common::Span;
use velt_sema::ide::{DefKind, DefRef};

use crate::analysis::Analysis;

/// The definition named at byte `offset` of the document.
pub fn def_at(analysis: &Analysis, offset: u32) -> Option<DefRef> {
    let ide = analysis.ide.as_ref()?;
    ide.def_at(analysis.file(), offset)
        .filter(|d| d.span != Span::DUMMY)
}

/// The type of the innermost expression at `offset`.
pub fn type_at(analysis: &Analysis, offset: u32) -> Option<String> {
    analysis.ide.as_ref()?.type_at(analysis.file(), offset)
}

/// Completion items for the names visible at `offset` (empty if sema has no analysis).
pub fn scope_items(analysis: &Analysis, offset: u32) -> Vec<CompletionItem> {
    let Some(ide) = analysis.ide.as_ref() else {
        return vec![];
    };
    ide.scope_at(analysis.file(), offset)
        .into_iter()
        .map(|(name, d)| def_item(analysis, &name, &d))
        .collect()
}

/// Members of `receiver` (a name visible at `offset`, `this`, or a namespace import); `None` when
/// sema does not know the name.
pub fn member_items(
    analysis: &Analysis,
    receiver: &str,
    offset: u32,
) -> Option<Vec<CompletionItem>> {
    let ide = analysis.ide.as_ref()?;
    let found = ide
        .scope_at(analysis.file(), offset)
        .into_iter()
        .find(|(name, _)| name == receiver);
    let Some((_, def)) = found else {
        return namespace_items(ide, analysis, receiver);
    };
    let members = ide.members_of(&def);
    if members.is_empty() {
        return None;
    }
    Some(
        members
            .into_iter()
            .filter(|(_, d, _)| d.kind != DefKind::Constructor)
            .map(|(name, d, _)| def_item(analysis, &name, &d))
            .collect(),
    )
}

/// The exports of namespace import `ns` (`import * as ns`); `None` if it is not one.
fn namespace_items(
    ide: &velt_sema::ide::Analysis,
    analysis: &Analysis,
    ns: &str,
) -> Option<Vec<CompletionItem>> {
    let members = ide.namespace_members(analysis.file(), ns);
    (!members.is_empty()).then(|| {
        members
            .into_iter()
            .map(|(name, d)| def_item(analysis, &name, &d))
            .collect()
    })
}

pub fn kind(k: DefKind) -> CompletionItemKind {
    match k {
        DefKind::Function | DefKind::ExternFunction => CompletionItemKind::FUNCTION,
        DefKind::Method | DefKind::StaticMethod => CompletionItemKind::METHOD,
        DefKind::Getter | DefKind::Field => CompletionItemKind::FIELD,
        DefKind::Constructor => CompletionItemKind::CONSTRUCTOR,
        DefKind::StaticField | DefKind::Constant => CompletionItemKind::CONSTANT,
        DefKind::Struct => CompletionItemKind::STRUCT,
        DefKind::Class => CompletionItemKind::CLASS,
        DefKind::Interface => CompletionItemKind::INTERFACE,
        DefKind::Enum => CompletionItemKind::ENUM,
        DefKind::Variant => CompletionItemKind::ENUM_MEMBER,
        DefKind::TypeAlias => CompletionItemKind::TYPE_PARAMETER,
        DefKind::Local | DefKind::Parameter => CompletionItemKind::VARIABLE,
    }
}

pub fn item(label: &str, kind: CompletionItemKind, detail: &str) -> CompletionItem {
    CompletionItem {
        label: label.to_string(),
        kind: Some(kind),
        detail: (!detail.is_empty()).then(|| detail.to_string()),
        ..Default::default()
    }
}

/// The completion item for definition `d` named `label`, carrying what `completionItem/resolve`
/// needs for its doc and tagged when it is deprecated ([`crate::docs::attach`]).
pub fn def_item(analysis: &Analysis, label: &str, d: &DefRef) -> CompletionItem {
    let mut out = item(label, kind(d.kind), &d.detail);
    crate::docs::attach(analysis, &mut out, d);
    out
}

/// The identifier around `offset` in `text` (the hover range).
pub fn word_at(text: &str, offset: u32) -> Option<(u32, u32)> {
    let is_ident = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '$';
    let o = (offset as usize).min(text.len());
    let lo = text[..o]
        .char_indices()
        .rev()
        .take_while(|(_, c)| is_ident(*c))
        .last()
        .map_or(o, |(i, _)| i);
    let hi = o + text[o..]
        .chars()
        .take_while(|c| is_ident(*c))
        .map(char::len_utf8)
        .sum::<usize>();
    (hi > lo).then_some((lo as u32, hi as u32))
}
