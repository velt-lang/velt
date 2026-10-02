//! Completion inside JSX. The context is read from the text before the cursor (while typing, the
//! element usually does not parse): right after `<` the tags (the intrinsic elements of the
//! file's JSX runtime, then the components in scope); inside an opening tag after its name the
//! attributes of that tag (intrinsic: the fields of `JSX.IntrinsicElements[tag]`; component: the
//! fields of its props parameter), minus the ones already written. Answers come from sema's IDE
//! queries ([`velt_sema::ide`]).

use lsp_types::{CompletionItem, CompletionItemKind};
use velt_sema::ide::{DefKind, DefRef};
use velt_syntax::ast;

use crate::analysis::Analysis;
use crate::index::pattern_idents;
use crate::index::scope::is_component;

use crate::sema_query::{item, kind};

/// How far back an opening tag is looked for.
const MAX_TAG_BYTES: usize = 4096;

/// Where in JSX the cursor is.
#[derive(Debug, PartialEq, Eq)]
pub enum Context<'t> {
    /// Typing a tag name after `<` or `</`.
    Tag,
    /// Typing an attribute name in the opening tag of `tag`; `written` are the attributes
    /// already there.
    Attribute { tag: &'t str, written: Vec<&'t str> },
}

/// The JSX context of a word starting at byte `word_start` of `text`, if any.
pub fn context(text: &str, word_start: usize) -> Option<Context<'_>> {
    let before = &text[..word_start];
    if before.ends_with("</") {
        return Some(Context::Tag);
    }
    if let Some(lt) = before.strip_suffix('<') {
        return tag_may_start(lt).then_some(Context::Tag);
    }
    if !before.ends_with(char::is_whitespace) || before.trim_end().ends_with('=') {
        return None;
    }
    let floor = word_start.saturating_sub(MAX_TAG_BYTES);
    for (i, _) in before.match_indices('<').rev() {
        if i < floor {
            break;
        }
        if !tag_may_start(&text[..i]) {
            continue;
        }
        match opening_tag(&before[i + 1..]) {
            Tag::Open(tag, written) => return Some(Context::Attribute { tag, written }),
            Tag::Closed => return None,
            Tag::Other => {}
        }
    }
    None
}

/// Can a `<` after `text` open an element: where an expression starts, or in the text between
/// the tags of an element?
fn tag_may_start(text: &str) -> bool {
    let floor = text.len().saturating_sub(MAX_TAG_BYTES);
    may_start_above(text, floor)
}

/// [`tag_may_start`], looking no further back than byte `floor`.
fn may_start_above(text: &str, floor: usize) -> bool {
    starts_element(text) || in_jsx_text(text, floor)
}

/// Does `text` end in JSX text: after a tag (`<p>`, `<a href="x">`, `<br />`, `</b>`, `<>`)
/// or a `{…}` child that is itself in JSX text, with only text since? Text may hold anything
/// but `<`, `>`, `{` and `}`. A `>` that compares, ends an arrow or closes type arguments
/// (`Array<i64>`) is not a tag, and a `}` that closes a block is not a child.
fn in_jsx_text(text: &str, floor: usize) -> bool {
    let Some(i) = text.rfind(['<', '>', '{', '}']).filter(|&i| i >= floor) else {
        return false;
    };
    match text.as_bytes()[i] {
        b'}' => {
            matching_brace(&text[..i], floor).is_some_and(|open| in_jsx_text(&text[..open], floor))
        }
        b'>' => closes_tag(&text[..i], floor),
        _ => false,
    }
}

/// Where the `{` is that a `}` right after `text` closes (not before `floor`).
fn matching_brace(text: &str, floor: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (i, c) in text.char_indices().rev() {
        if i < floor {
            return None;
        }
        match c {
            '}' => depth += 1,
            '{' if depth == 0 => return Some(i),
            '{' => depth -= 1,
            _ => {}
        }
    }
    None
}

/// Does a `>` right after `text` close a tag: `<>`, `</name>`, or an opening or self-closing
/// tag whose `<` can open an element?
fn closes_tag(text: &str, floor: usize) -> bool {
    if text.ends_with('<') || text.ends_with("</") {
        return true;
    }
    for (lt, _) in text.match_indices('<').rev() {
        if lt < floor {
            return false;
        }
        let inside = &text[lt + 1..];
        if let Some(name) = inside.strip_prefix('/') {
            return !name.is_empty() && name.chars().all(is_name_char);
        }
        if let Tag::Open(..) = opening_tag(inside) {
            return may_start_above(&text[..lt], floor);
        }
    }
    false
}

/// Can an element start after `text` (an operand is expected: not after a name, `)` or `]`,
/// where `<` compares or starts type arguments, but after a keyword such as `return`)?
fn starts_element(text: &str) -> bool {
    let text = text.trim_end();
    let word_start = text
        .char_indices()
        .rev()
        .take_while(|(_, c)| c.is_ascii_alphanumeric() || *c == '_' || *c == '$')
        .last()
        .map_or(text.len(), |(i, _)| i);
    let word = &text[word_start..];
    if !word.is_empty() {
        return OPERAND_KEYWORDS.contains(&word);
    }
    !text.ends_with([')', ']', '<'])
}

/// Keywords after which an expression starts.
const OPERAND_KEYWORDS: &[&str] = &[
    "return", "await", "yield", "throw", "case", "else", "do", "in", "of", "default",
];

enum Tag<'t> {
    /// Still open at the end: its name and the attribute names written so far.
    Open(&'t str, Vec<&'t str>),
    /// Its `>` came before the end.
    Closed,
    /// Not an element (a comparison, a type argument list, unbalanced braces or quotes).
    Other,
}

/// `rest` follows a `<`: the tag it opens, if it is still open at the end of `rest`.
fn opening_tag(rest: &str) -> Tag<'_> {
    let name_len = rest.find(|c: char| !is_name_char(c)).unwrap_or(rest.len());
    if name_len == 0 {
        return Tag::Other;
    }
    let (name, attrs) = rest.split_at(name_len);
    let (mut depth, mut quote) = (0usize, None);
    let mut written = vec![];
    let mut word: Option<usize> = None;
    for (i, c) in attrs.char_indices() {
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
            continue;
        }
        let in_word = depth == 0 && is_name_char(c);
        match (in_word, word) {
            (true, None) => word = Some(i),
            (false, Some(start)) => {
                written.push(&attrs[start..i]);
                word = None;
            }
            _ => {}
        }
        match c {
            '"' | '\'' | '`' => quote = Some(c),
            '{' => depth += 1,
            '}' if depth == 0 => return Tag::Other,
            '}' => depth -= 1,
            '>' if depth == 0 => return Tag::Closed,
            _ => {}
        }
    }
    if quote.is_some() || depth > 0 {
        return Tag::Other;
    }
    Tag::Open(name, written)
}

fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '$' | '-' | ':' | '.')
}

/// Completion items for `ctx` at byte `offset` of the document.
pub fn items(analysis: &Analysis, ctx: &Context<'_>, offset: u32) -> Vec<CompletionItem> {
    let Some(ide) = analysis.ide.as_ref() else {
        return vec![];
    };
    let file = analysis.file();
    match ctx {
        Context::Tag => {
            let tags = ide.jsx_intrinsics(file).iter();
            let mut out: Vec<CompletionItem> = tags
                .map(|(tag, _, ty)| item(tag, CompletionItemKind::PROPERTY, ty))
                .collect();
            let components = ide.scope_at(file, offset).into_iter().filter(|(name, d)| {
                is_component(name) && (d.kind == DefKind::Function || d.detail.contains("=>"))
            });
            out.extend(components.map(|(name, d)| item(&name, kind(d.kind), &d.detail)));
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
    }
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

#[cfg(test)]
mod tests {
    use super::{context, Context};

    fn ctx(text: &str) -> Option<Context<'_>> {
        context(text, text.len())
    }

    #[test]
    fn tags_and_attributes_in_jsx_text() {
        assert_eq!(ctx("<p>Read the <"), Some(Context::Tag));
        assert_eq!(ctx("<p>hello</"), Some(Context::Tag));
        assert_eq!(ctx("<p>Hello, world? {name} and <"), Some(Context::Tag));
        assert_eq!(
            ctx("<p>Read the <a "),
            Some(Context::Attribute {
                tag: "a",
                written: vec![]
            })
        );
        assert_eq!(ctx("if (a > b && c <"), None);
        assert_eq!(
            ctx("function f() {}
if (a <"),
            None
        );
        assert_eq!(
            ctx("if (x) { g(); }
while (i <"),
            None
        );
        assert_eq!(ctx("const xs: Array<i64> = f(); if (n <"), None);
        assert_eq!(ctx("const n = i<"), None);
        assert_eq!(ctx("<p><br />then <"), Some(Context::Tag));
        assert_eq!(ctx("<ul><li>a</li>and <"), Some(Context::Tag));
        assert_eq!(ctx("<>frag <"), Some(Context::Tag));
        assert_eq!(ctx("const ok = x >= 1 && y <"), None);
        // Non-ASCII text before the cursor.
        assert_eq!(ctx("<p>Grüße 😀 <"), Some(Context::Tag));
        assert_eq!(ctx("const total = größe <"), None);
    }

    #[test]
    fn quotes_inside_braces_do_not_unbalance_them() {
        assert_eq!(
            ctx("<a href={() => \"}\"} "),
            Some(Context::Attribute {
                tag: "a",
                written: vec!["href"]
            })
        );
    }

    #[test]
    fn tags_after_a_less_than_where_an_operand_starts() {
        assert_eq!(ctx("return <"), Some(Context::Tag));
        assert_eq!(ctx("<div>\n  </"), Some(Context::Tag));
        assert_eq!(ctx("const ok = a <"), None);
        assert_eq!(ctx("f(x) <"), None);
    }

    #[test]
    fn attributes_inside_an_open_tag() {
        assert_eq!(
            ctx("return <a "),
            Some(Context::Attribute {
                tag: "a",
                written: vec![]
            })
        );
        assert_eq!(
            ctx("<Card title=\"a > b\" on={() => x > 1} hidden "),
            Some(Context::Attribute {
                tag: "Card",
                written: vec!["title", "on", "hidden"]
            })
        );
        assert_eq!(ctx("<a href="), None);
        assert_eq!(ctx("<a href=\"x "), None);
        assert_eq!(ctx("<a href=\"x\">text "), None);
        assert_eq!(ctx("if (a < b && c "), None);
    }
}
