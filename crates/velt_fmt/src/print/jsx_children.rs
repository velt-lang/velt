//! JSX children as prettier prints them (`printJsxChildren`): text is split into words that a
//! paragraph fill reflows, and the separators between words, tags and `{…}` follow from React's
//! whitespace rules so that the rendered text never changes:
//! - between two words, a space or a line break (both cook to one space);
//! - a space that matters next to a tag or `{…}` is a [`Part::Space`]: a plain space while the
//!   line holds, `{" "}` followed by a line break where the line breaks;
//! - where the source has no space, or only whitespace containing a line break, next to a tag or
//!   `{…}`, a line break may go (it cooks to nothing); between two tags or `{…}` one always does.
//!
//! Text is printed from the source, so entities stay as written. Unlike prettier, runs of several
//! spaces within a line are kept (prettier collapses them, which changes the text): between words
//! they stay inside one unbreakable word, next to a tag they become `{"  "}` where the line breaks.
//! What happens around these parts (merging, `{" "}` at the edges) is in [`super::jsx_layout`].

use velt_syntax::ast::{ExprKind, JsxChild, JsxElement, Lit};

use super::jsx::is_self_closing;
use super::Printer;
use crate::doc::{cat, concat, hardline, nil, text, Doc};
use crate::source::slice;

/// One entry of the children list: contents at even indices, separators at odd ones (prettier's
/// `fill` rule).
#[derive(Clone, Debug)]
pub(super) enum Part {
    /// No content (prettier's `""`).
    Empty,
    /// A word of text or a printed element or `{…}`.
    Content(Doc),
    /// A space or a line break, between two words.
    Line,
    /// Nothing or a line break.
    Soft,
    /// A line break.
    Hard,
    /// Spaces that matter (cooked), next to a tag, a `{…}` or the element's edge.
    Space(String),
}

impl Part {
    pub(super) fn is_line(&self) -> bool {
        matches!(self, Part::Soft | Part::Hard)
    }
}

/// A child as prettier sees it: the AST drops whitespace-only text containing a line break, and
/// `{" "}` counts as text.
#[derive(Clone, Copy)]
pub(super) enum Child<'a> {
    /// Raw source text.
    Text(&'a str),
    Node(&'a JsxChild),
}

/// JSX whitespace (React trims and joins on these only).
fn is_jsx_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r')
}

/// Text with something other than whitespace, or whitespace without a line break.
pub(super) fn is_meaningful(raw: &str) -> bool {
    raw.contains(|c| !is_jsx_space(c)) || !raw.contains(['\n', '\r'])
}

/// A whitespace run within a line, cooked (tabs count as spaces).
fn cooked(run: &str) -> String {
    run.replace('\t', " ")
}

/// The words of a text and the whitespace before the first and after the last.
struct Words<'a> {
    lead: Option<&'a str>,
    words: Vec<String>,
    trail: Option<&'a str>,
}

/// Splits raw text on JSX whitespace. A run of several spaces within a line keeps the words
/// around it together (breaking there, or printing one space, would change the text).
fn split_words(raw: &str) -> Words<'_> {
    let mut pieces: Vec<&str> = vec![];
    let mut rest = raw;
    while !rest.is_empty() {
        let space = rest.len() - rest.trim_start_matches(is_jsx_space).len();
        let len = if space > 0 {
            space
        } else {
            rest.find(is_jsx_space).unwrap_or(rest.len())
        };
        pieces.push(&rest[..len]);
        rest = &rest[len..];
    }
    let is_space = |p: &str| p.starts_with(is_jsx_space);
    let lead = pieces.first().copied().filter(|p| is_space(p));
    let trail = pieces
        .last()
        .copied()
        .filter(|p| pieces.len() > 1 && is_space(p));
    let body = &pieces[usize::from(lead.is_some())..pieces.len() - usize::from(trail.is_some())];
    let mut words: Vec<String> = vec![];
    let mut glue = false;
    for piece in body {
        if is_space(piece) {
            glue = !piece.contains(['\n', '\r']) && piece.chars().count() > 1;
            if glue {
                if let Some(last) = words.last_mut() {
                    last.push_str(&cooked(piece));
                }
            }
            continue;
        }
        match words.last_mut() {
            Some(last) if glue => last.push_str(piece),
            _ => words.push(piece.to_string()),
        }
        glue = false;
    }
    Words { lead, words, trail }
}

/// Is `child` an element written `<x />` (prettier: a `JSXElement` without closing element)?
fn self_closing(src: &str, child: Option<Child<'_>>) -> bool {
    matches!(child, Some(Child::Node(JsxChild::Element(el)))
        if el.name.is_some() && el.children.is_empty() && is_self_closing(slice(src, el.span)))
}

/// Length as JavaScript counts it (UTF-16 units).
fn js_len(word: &str) -> usize {
    word.encode_utf16().count()
}

/// Separator where no whitespace is written (prettier's `separatorNoWhitespace`).
fn separator_no_whitespace(word: &str, closes_itself: bool) -> Part {
    if closes_itself && js_len(word) != 1 {
        Part::Hard
    } else {
        Part::Soft
    }
}

/// Separator for whitespace with a line break (prettier's `separatorWithWhitespace`).
fn separator_with_whitespace(word: &str, closes_itself: bool) -> Part {
    if js_len(word) == 1 && !closes_itself {
        Part::Soft
    } else {
        Part::Hard
    }
}

/// Builds the parts list with prettier's `push` / `pushLine`.
struct Builder {
    parts: Vec<Part>,
}

impl Builder {
    fn push(&mut self, doc: Doc) {
        let joined = match self.parts.pop() {
            Some(Part::Content(prev)) => cat![prev, doc],
            _ => doc,
        };
        self.parts.push(Part::Content(joined));
    }

    fn push_line(&mut self, sep: Part) {
        self.parts.push(sep);
        self.parts.push(Part::Empty);
    }

    /// A text child; `next_closes`: is the next child an element written `<x />`?
    fn text(&mut self, raw: &str, next_closes: bool) {
        if !is_meaningful(raw) {
            if raw.matches('\n').count() > 1 {
                self.push_line(Part::Hard);
            }
            return;
        }
        let Words { lead, words, trail } = split_words(raw);
        if let Some(lead) = lead {
            self.push_line(if lead.contains(['\n', '\r']) {
                let first = words.first().map_or("", String::as_str);
                separator_with_whitespace(first, next_closes)
            } else {
                Part::Space(cooked(lead))
            });
        }
        if words.is_empty() {
            return;
        }
        for (i, word) in words.iter().enumerate() {
            if i > 0 {
                self.push_line(Part::Line);
            }
            self.push(text(word.as_str()));
        }
        let last = words.last().map_or("", String::as_str);
        let sep = match trail {
            Some(t) if t.contains(['\n', '\r']) => separator_with_whitespace(last, next_closes),
            Some(t) => Part::Space(cooked(t)),
            None => separator_no_whitespace(last, next_closes),
        };
        self.push_line(sep);
    }
}

/// The source of a `{" "}` child, as prettier recognises it.
fn is_space_container(src: &str, child: &JsxChild) -> bool {
    let JsxChild::Expr {
        expr: Some(expr),
        span,
    } = child
    else {
        return false;
    };
    let ExprKind::Lit(Lit::Str(value)) = &expr.kind else {
        return false;
    };
    let inner = slice(src, *span)
        .strip_prefix('{')
        .and_then(|s| s.strip_suffix('}'))
        .unwrap_or("")
        .trim_matches(is_jsx_space);
    value == " " && (inner == "\" \"" || inner == "' '")
}

impl<'a> Printer<'a> {
    /// The children of `el` as prettier sees them: the AST's children, `{" "}` as text (unless
    /// the children must stay `exact`, see [`super::jsx_layout`]), and the whitespace the parser
    /// dropped (it decides on line breaks and blank lines).
    pub(super) fn jsx_virtual_children<'e>(&self, el: &'e JsxElement, exact: bool) -> Vec<Child<'e>>
    where
        'a: 'e,
    {
        let src: &'e str = self.src;
        let mut out = vec![];
        let mut prev: Option<(u32, bool)> = None;
        for child in &el.children {
            let (lo, hi, is_text) = child_bounds(child);
            let (gap_lo, prev_text) = match prev {
                Some(p) => p,
                None => ((lo as usize - leading_space_len(src, lo)) as u32, false),
            };
            if !is_text && !prev_text && gap_lo < lo {
                out.push(Child::Text(&src[gap_lo as usize..lo as usize]));
            }
            out.push(if is_text {
                Child::Text(&src[lo as usize..hi as usize])
            } else if !exact && is_space_container(src, child) {
                Child::Text(" ")
            } else {
                Child::Node(child)
            });
            prev = Some((hi, is_text));
        }
        if let Some((hi, false)) = prev {
            let end = hi as usize + trailing_space_len(src, hi);
            if end > hi as usize {
                out.push(Child::Text(&src[hi as usize..end]));
            }
        }
        out
    }

    /// The parts list of prettier's `printJsxChildren`.
    pub(super) fn jsx_parts(&mut self, children: &[Child<'_>]) -> Vec<Part> {
        let mut b = Builder {
            parts: vec![Part::Empty],
        };
        for (i, child) in children.iter().enumerate() {
            let next = children.get(i + 1).copied();
            let next_closes = self_closing(self.src, next);
            match child {
                Child::Text(raw) => b.text(raw, next_closes),
                Child::Node(node) => {
                    let doc = self.jsx_child_node(node);
                    b.push(doc);
                    let closes = next_closes || self_closing(self.src, Some(*child));
                    let sep = match next {
                        Some(Child::Text(raw)) if is_meaningful(raw) => {
                            let first = split_words(raw.trim_matches(is_jsx_space)).words;
                            let first = first.first().map_or("", String::as_str);
                            separator_no_whitespace(first, closes)
                        }
                        _ => Part::Hard,
                    };
                    b.push_line(sep);
                }
            }
        }
        b.parts
    }

    /// An element or a `{…}` child.
    fn jsx_child_node(&mut self, child: &JsxChild) -> Doc {
        match child {
            JsxChild::Element(el) => self.jsx_element(el),
            JsxChild::Expr {
                expr: Some(expr),
                span,
            } => self.jsx_container(expr, span.hi),
            JsxChild::Expr { expr: None, span } => {
                // `jsx_dangling` puts a space before each comment: `{ /* note */}` reads
                // badly, so it is dropped after the brace.
                let comments = self.comments.take_before(span.hi);
                let docs: Vec<Doc> = comments
                    .iter()
                    .enumerate()
                    .map(|(i, c)| {
                        let before = if i == 0 { nil() } else { text(" ") };
                        let after = if c.needs_newline() { hardline() } else { nil() };
                        cat![before, text(c.text.clone()), after]
                    })
                    .collect();
                cat!["{", concat(docs), "}"]
            }
            JsxChild::Spread { expr, span } => self.jsx_spread(expr, span.hi),
            JsxChild::Text { .. } => nil(),
        }
    }
}

/// `(lo, hi, is text)` of a child in the source.
pub(super) fn child_bounds(child: &JsxChild) -> (u32, u32, bool) {
    match child {
        JsxChild::Text { span, .. } => (span.lo, span.hi, true),
        JsxChild::Expr { span, .. } | JsxChild::Spread { span, .. } => (span.lo, span.hi, false),
        JsxChild::Element(el) => (el.span.lo, el.span.hi, false),
    }
}

/// Length of the JSX whitespace right before `pos` (after the opening tag's `>`).
fn leading_space_len(src: &str, pos: u32) -> usize {
    let before = &src[..pos as usize];
    before.len() - before.trim_end_matches(is_jsx_space).len()
}

/// Length of the JSX whitespace from `pos` on (up to the closing tag's `<`).
fn trailing_space_len(src: &str, pos: u32) -> usize {
    let after = &src[pos as usize..];
    after.len() - after.trim_start_matches(is_jsx_space).len()
}

#[cfg(test)]
mod tests {
    use super::split_words;

    #[test]
    fn several_spaces_keep_words_together() {
        let w = split_words("  a   b c\n  d\t");
        assert_eq!(w.lead, Some("  "));
        assert_eq!(w.words, ["a   b", "c", "d"]);
        assert_eq!(w.trail, Some("\t"));
        let w = split_words("x\t\ty");
        assert_eq!(w.words, ["x  y"]);
        assert!(split_words(" ").words.is_empty());
    }
}
