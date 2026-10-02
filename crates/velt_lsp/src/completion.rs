//! Completion. In JSX: tags and attributes ([`jsx_completion`]). After `receiver.`: the members
//! of the receiver's type (`this` → the enclosing class). Elsewhere: locals in scope, the module's items, imported names, prelude items, built-in
//! globals and keywords. The receiver is read from the text (while typing `x.` the statement usually
//! does not parse) and looked up among the names sema sees at the cursor; the AST index answers
//! when sema has no analysis or does not know the name.

use std::collections::HashSet;

use lsp_types::{CompletionItem, CompletionItemKind};
use velt_syntax::ast;

use crate::analysis::Analysis;
use crate::index::scope;
use crate::index::{self, Decl, DeclKind};
use crate::signature;
use crate::{definition, jsx_completion, sema_query};

/// Keywords and built-in type names (whitespace separated).
const KEYWORDS: &str = "async await break case catch class const constructor continue declare \
    default do else enum export extends false finally for from function if implements import in \
    interface let new null of return shared static struct switch this throw true try type void \
    while as readonly override super i8 i16 i32 i64 isize u8 u16 u32 u64 usize f32 f64 number \
    bool string";

/// Globals the compiler provides without a declaration in the prelude.
const BUILTINS: &[&str] = &[
    "console",
    "process",
    "panic",
    "shared",
    "spawn",
    "sleep",
    "yieldNow",
    "Promise",
    "performance",
    "Date",
    "JSON",
    "Math",
    "Map",
];

/// Completion items at byte `offset` of the document; with `jsx_only` (completion triggered by
/// `<`), nothing outside JSX.
pub fn complete(analysis: &Analysis, offset: u32, jsx_only: bool) -> Vec<CompletionItem> {
    let text = analysis.text();
    let offset = (offset as usize).min(text.len());
    let word_start = text[..offset]
        .char_indices()
        .rev()
        .take_while(|(_, c)| is_ident_char(*c))
        .last()
        .map_or(offset, |(i, _)| i);
    let at = word_start as u32;
    if let Some(ctx) = jsx_completion::context(text, word_start) {
        return jsx_completion::items(analysis, &ctx, at);
    }
    if jsx_only {
        return vec![];
    }
    if let Some(receiver) = receiver_before(&text[..word_start]) {
        if let Some(items) = sema_query::member_items(analysis, receiver, at) {
            return items;
        }
        let info = scope::at_offset(analysis, at);
        return member_items(analysis, receiver, &info, at);
    }
    let mut from_sema = sema_query::scope_items(analysis, at);
    if from_sema.is_empty() {
        let info = scope::at_offset(analysis, at);
        return scope_items(analysis, &info);
    }
    from_sema.extend(namespace_items(analysis));
    with_builtins(from_sema)
}

/// The document's namespace imports (`import * as ns`), which sema does not list as names.
fn namespace_items(analysis: &Analysis) -> Vec<CompletionItem> {
    let imports = analysis
        .module()
        .ast
        .items
        .iter()
        .filter_map(|it| match &it.kind {
            ast::ItemKind::Import(import) => Some(import),
            _ => None,
        });
    imports
        .filter_map(|import| {
            let ns = import.namespace.as_ref()?;
            let detail = format!("import * as {} from \"{}\"", ns.name, import.from);
            Some(item(&ns.name, CompletionItemKind::MODULE, &detail))
        })
        .collect()
}

/// Built-in globals and keywords after the named items (skipping names already offered).
fn with_builtins(mut out: Vec<CompletionItem>) -> Vec<CompletionItem> {
    let mut seen: HashSet<String> = out.iter().map(|i| i.label.clone()).collect();
    for name in BUILTINS.iter().filter(|n| seen.insert(n.to_string())) {
        out.push(item(name, CompletionItemKind::MODULE, "built-in"));
    }
    for kw in KEYWORDS.split_whitespace().filter(|k| !seen.contains(*k)) {
        out.push(item(kw, CompletionItemKind::KEYWORD, ""));
    }
    out
}

/// Is `word` a keyword (or built-in type name)?
pub fn is_keyword(word: &str) -> bool {
    KEYWORDS.split_whitespace().any(|k| k == word)
}

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '$'
}

/// The identifier before a trailing `.` / `?.`, e.g. `user` in `…user.` (`None` if not a member).
fn receiver_before(text: &str) -> Option<&str> {
    let before_dot = text.trim_end().strip_suffix('.')?;
    let before_dot = before_dot
        .strip_suffix('?')
        .unwrap_or(before_dot)
        .trim_end();
    let start = before_dot
        .char_indices()
        .rev()
        .take_while(|(_, c)| is_ident_char(*c))
        .last()
        .map(|(i, _)| i)?;
    Some(&before_dot[start..])
}

fn member_items(
    analysis: &Analysis,
    receiver: &str,
    info: &scope::CursorInfo,
    offset: u32,
) -> Vec<CompletionItem> {
    let namespace = (receiver != "this" && !info.visible.iter().any(|b| b.name == receiver))
        .then(|| index::namespace_target(analysis, analysis.root, receiver))
        .flatten();
    if let Some(target) = namespace {
        return index::module_items(analysis, target, true)
            .iter()
            .map(|d| decl_item(analysis, d))
            .collect();
    }
    let ty = if receiver == "this" {
        info.owner
            .or_else(|| enclosing_type(analysis, offset))
            .and_then(|owner| definition::owner_decl(analysis, owner))
    } else {
        definition::named_receiver_type(analysis, receiver, &info.visible)
    };
    let Some(ty) = ty else { return vec![] };
    let mut seen = HashSet::new();
    index::members(analysis, &ty)
        .into_iter()
        .filter(|m| !matches!(m.kind, DeclKind::Constructor(..)) && seen.insert(m.name.clone()))
        .map(|m| decl_item(analysis, &m))
        .collect()
}

/// Fallback when the walk found no owner (the member statement did not parse): the top-level
/// class, struct or interface containing the cursor.
fn enclosing_type(analysis: &Analysis, offset: u32) -> Option<&ast::Item> {
    analysis.module().ast.items.iter().find(|i| {
        matches!(
            i.kind,
            ast::ItemKind::Class(_) | ast::ItemKind::Struct(_) | ast::ItemKind::Interface(_)
        ) && i.span.lo <= offset
            && offset <= i.span.hi
    })
}

fn scope_items(analysis: &Analysis, info: &scope::CursorInfo) -> Vec<CompletionItem> {
    let mut seen = HashSet::new();
    let mut out = vec![];
    for local in info.visible.iter().rev() {
        if seen.insert(local.name.clone()) {
            out.push(item(
                &local.name,
                CompletionItemKind::VARIABLE,
                &local.detail,
            ));
        }
    }
    let root = analysis.root;
    let mut globals = index::module_items(analysis, root, false);
    globals.extend(index::imports(analysis, root).filter_map(|(import, name)| {
        let target = index::import_target(analysis, root, &import.from)?;
        let mut d = index::item_in(analysis, target, &name.name.name, true)?;
        d.name = name.alias.as_ref().unwrap_or(&name.name).name.clone();
        Some(d)
    }));
    out.extend(namespace_items(analysis));
    for prelude in index::prelude_modules(analysis) {
        globals.extend(index::module_items(analysis, prelude, true));
    }
    for d in globals {
        if seen.insert(d.name.clone()) {
            out.push(decl_item(analysis, &d));
        }
    }
    with_builtins(out)
}

fn decl_item(analysis: &Analysis, d: &Decl) -> CompletionItem {
    item(&d.name, kind(d), &signature::decl(analysis, d))
}

fn kind(d: &Decl) -> CompletionItemKind {
    match &d.kind {
        DeclKind::Item(item) => match &item.kind {
            ast::ItemKind::Function(_) | ast::ItemKind::ExternFn(_) => CompletionItemKind::FUNCTION,
            ast::ItemKind::Class(_) => CompletionItemKind::CLASS,
            ast::ItemKind::Struct(_) => CompletionItemKind::STRUCT,
            ast::ItemKind::Interface(_) => CompletionItemKind::INTERFACE,
            ast::ItemKind::Enum(_) => CompletionItemKind::ENUM,
            ast::ItemKind::TypeAlias(_) => CompletionItemKind::TYPE_PARAMETER,
            ast::ItemKind::Var(v) if v.kind == ast::VarKind::Const => CompletionItemKind::CONSTANT,
            _ => CompletionItemKind::VARIABLE,
        },
        DeclKind::Field(..) => CompletionItemKind::FIELD,
        DeclKind::Method(..) => CompletionItemKind::METHOD,
        DeclKind::Constructor(..) => CompletionItemKind::CONSTRUCTOR,
        DeclKind::Variant(..) => CompletionItemKind::ENUM_MEMBER,
        DeclKind::Local(_) => CompletionItemKind::VARIABLE,
    }
}

fn item(label: &str, kind: CompletionItemKind, detail: &str) -> CompletionItem {
    sema_query::item(label, kind, detail)
}
