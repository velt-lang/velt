//! Where in JSX the cursor is, read from the text before it (while typing, the element usually
//! does not parse): after `<` where an element can start (an expression starts there, or the
//! text between tags goes on), after `</`, or inside an opening tag after its name.

/// How far back an opening tag is looked for.
const MAX_TAG_BYTES: usize = 4096;

/// Where in JSX the cursor is.
#[derive(Debug, PartialEq, Eq)]
pub enum Context<'t> {
    /// Typing a tag name after `<` or `</`; after `</`, `closing` is the innermost element
    /// still open there (offered first).
    Tag { closing: Option<&'t str> },
    /// Typing an attribute name in the opening tag of `tag`; `written` are the attributes
    /// already there.
    Attribute { tag: &'t str, written: Vec<&'t str> },
}

/// The JSX context of a word starting at byte `word_start` of `text`, if any.
pub fn context(text: &str, word_start: usize) -> Option<Context<'_>> {
    let before = &text[..word_start];
    if let Some(lt) = before.strip_suffix("</") {
        return Some(Context::Tag {
            closing: innermost_open(lt),
        });
    }
    if let Some(lt) = before.strip_suffix('<') {
        return tag_may_start(lt).then_some(Context::Tag { closing: None });
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
            Tag::Closed { .. } => return None,
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
    // After `)`, `]`, a string or a postfix `!` an operand just ended: `<` compares.
    !text.ends_with([')', ']', '<', '"', '\'', '`', '!'])
}

/// Keywords after which an expression starts.
const OPERAND_KEYWORDS: &[&str] = &[
    "return", "await", "yield", "throw", "case", "else", "do", "in", "of", "default",
];

enum Tag<'t> {
    /// Still open at the end: its name and the attribute names written so far.
    Open(&'t str, Vec<&'t str>),
    /// Its `>` came before the end, at byte `end` of the text after the `<`; `self_closing`
    /// for `/>`.
    Closed { end: usize, self_closing: bool },
    /// Not an element (a comparison, a type argument list, unbalanced braces or quotes).
    Other,
}

/// `rest` follows a `<`: the tag it opens, if it is still open at the end of `rest`. Strings
/// and comments (`{/* … */}`, `// …`) are skipped.
fn opening_tag(rest: &str) -> Tag<'_> {
    let name_len = rest.find(|c: char| !is_name_char(c)).unwrap_or(rest.len());
    if name_len == 0 {
        return Tag::Other;
    }
    let (name, attrs) = rest.split_at(name_len);
    let (mut depth, mut quote) = (0usize, None);
    let mut written = vec![];
    let mut word: Option<usize> = None;
    let mut skip_to = 0;
    for (i, c) in attrs.char_indices() {
        if i < skip_to {
            continue;
        }
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
            continue;
        }
        if let Some(end) = comment_end(&attrs[i..]) {
            skip_to = i + end;
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
            '>' if depth == 0 => {
                let self_closing = attrs[..i].ends_with('/');
                return Tag::Closed {
                    end: name_len + i + 1,
                    self_closing,
                };
            }
            _ => {}
        }
    }
    if quote.is_some() || depth > 0 {
        return Tag::Other;
    }
    Tag::Open(name, written)
}

/// The length of the comment `text` starts with, if it starts with one (an unclosed one runs to
/// the end).
fn comment_end(text: &str) -> Option<usize> {
    if let Some(body) = text.strip_prefix("/*") {
        return Some(body.find("*/").map_or(text.len(), |i| i + 4));
    }
    let body = text.strip_prefix("//")?;
    Some(body.find('\n').map_or(text.len(), |i| i + 2))
}

/// The innermost element still open at the end of `text` (from `MAX_TAG_BYTES` back): opening
/// tags push their name, closing tags pop to theirs, self-closing tags do neither.
fn innermost_open(text: &str) -> Option<&str> {
    let floor = text.len().saturating_sub(MAX_TAG_BYTES);
    let mut open: Vec<&str> = vec![];
    let mut i = floor;
    while let Some(lt) = text.get(i..).and_then(|t| t.find('<')).map(|lt| i + lt) {
        let rest = &text[lt + 1..];
        i = lt + 1;
        if let Some(name) = rest.strip_prefix('/') {
            let len = name.find(|c: char| !is_name_char(c)).unwrap_or(name.len());
            if let Some(at) = open.iter().rposition(|o| *o == &name[..len]) {
                open.truncate(at);
            }
            continue;
        }
        if !tag_may_start(&text[..lt]) {
            continue;
        }
        if let Tag::Closed { end, self_closing } = opening_tag(rest) {
            if !self_closing {
                let len = rest.find(|c: char| !is_name_char(c)).unwrap_or(rest.len());
                open.push(&rest[..len]);
            }
            i = lt + 1 + end;
        }
    }
    open.pop()
}

/// Where the word ending at `offset` starts, for completion. In an opening tag a word may hold
/// `-` (`aria-label`, `data-id`); `word_start` is where the identifier ends ordinary words.
pub fn word_start(text: &str, offset: usize, ident_start: usize) -> usize {
    let mut start = ident_start;
    while text[..start].ends_with('-') {
        let before = &text[..start - 1];
        let run = before.len()
            - before
                .trim_end_matches(|c: char| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                .len();
        if run == 0 {
            break;
        }
        start = start - 1 - run;
    }
    let attribute = matches!(context(text, start), Some(Context::Attribute { .. }));
    if start < ident_start && attribute && start <= offset {
        start
    } else {
        ident_start
    }
}

fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '$' | '-' | ':' | '.')
}

#[cfg(test)]
mod tests {
    use super::{context, Context};

    fn ctx(text: &str) -> Option<Context<'_>> {
        context(text, text.len())
    }

    #[test]
    fn tags_and_attributes_in_jsx_text() {
        assert_eq!(ctx("<p>Read the <"), Some(Context::Tag { closing: None }));
        assert_eq!(ctx("<p>hello</"), Some(Context::Tag { closing: Some("p") }));
        assert_eq!(
            ctx("<p>Hello, world? {name} and <"),
            Some(Context::Tag { closing: None })
        );
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
        assert_eq!(ctx("<p><br />then <"), Some(Context::Tag { closing: None }));
        assert_eq!(
            ctx("<ul><li>a</li>and <"),
            Some(Context::Tag { closing: None })
        );
        assert_eq!(ctx("<>frag <"), Some(Context::Tag { closing: None }));
        assert_eq!(ctx("const ok = x >= 1 && y <"), None);
        // Non-ASCII text before the cursor.
        assert_eq!(ctx("<p>Grüße 😀 <"), Some(Context::Tag { closing: None }));
        assert_eq!(ctx("const total = größe <"), None);
    }

    #[test]
    fn after_a_closing_slash_the_innermost_open_element() {
        fn closing(text: &str) -> Option<&str> {
            match ctx(text) {
                Some(Context::Tag { closing }) => closing,
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(closing("<ul><li><b>x</b><br /></"), Some("li"));
        assert_eq!(closing("<ul><li>a</li></"), Some("ul"));
        assert_eq!(closing("<Card title={a > b ? 1 : 2}></"), Some("Card"));
        assert_eq!(closing("<ui.Card><p>x</p></"), Some("ui.Card"));
        assert_eq!(
            closing(
                "const n = 1;
</"
            ),
            None
        );
    }

    #[test]
    fn a_string_or_a_postfix_bang_ends_an_operand() {
        assert_eq!(ctx("const t = \"a\" < b "), None);
        assert_eq!(ctx("if (x! < y && "), None);
        assert_eq!(ctx("f(\"a\" <"), None);
    }

    #[test]
    fn comments_inside_a_tag_are_skipped() {
        assert_eq!(
            ctx("<a {/* it's here */} href=\"x\" "),
            Some(Context::Attribute {
                tag: "a",
                written: vec!["href"]
            })
        );
    }

    #[test]
    fn hyphenated_attribute_names_are_one_word() {
        let text = "<div aria-la";
        let start = super::word_start(text, text.len(), text.len() - 2);
        assert_eq!(&text[start..], "aria-la");
        // Outside a tag `-` still separates words.
        let text = "const x = y-la";
        assert_eq!(
            super::word_start(text, text.len(), text.len() - 2),
            text.len() - 2
        );
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
        assert_eq!(ctx("return <"), Some(Context::Tag { closing: None }));
        assert_eq!(
            ctx("<div>\n  </"),
            Some(Context::Tag {
                closing: Some("div")
            })
        );
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
