//! JSX children as a paragraph fill (prettier's `printJsxChildren`). Text is printed from the
//! source, word by word, so entities stay as written; what may go between two pieces follows
//! from React's whitespace rules, so the cooked text never changes:
//! - between two words, a space or a line break (both cook to one space) — the text reflows;
//! - a space that matters next to a tag or `{…}` stays a space on the same line (breaking there
//!   would drop it);
//! - where the source has no space, or only whitespace containing a line break, next to a tag
//!   or `{…}`, a line break may go (it cooks to nothing); between two tags or `{…}`, and where
//!   the author already broke the line next to one, it always does.
//!
//! Runs of several spaces or tabs inside a line are kept as spaces.

use velt_syntax::ast::JsxChild;

use super::Printer;
use crate::doc::{cat, hardline, line, nil, softline, text, Doc};
use crate::source::slice;

/// What sits between a tag and the first or last child.
pub(super) enum Edge {
    /// Nothing that matters: a line break may go here.
    Break,
    /// Whitespace that matters (cooked), printed as is.
    Glue(String),
}

impl Edge {
    /// The glued whitespace, or nothing.
    pub(super) fn glue(self) -> Doc {
        match self {
            Edge::Glue(s) => text(s),
            Edge::Break => nil(),
        }
    }
}

/// Children ready for [`crate::doc::fill`].
pub(super) struct Children {
    pub(super) start: Edge,
    /// `[content, separator, content, …]`; empty when there are only spaces.
    pub(super) parts: Vec<Doc>,
    pub(super) end: Edge,
}

/// Whitespace seen since the last content.
enum Gap {
    None,
    /// Contains a line break: cooks to a single space between words, to nothing elsewhere.
    Newline,
    /// Spaces and tabs within a line, cooked (tabs as spaces).
    Spaces(String),
}

/// Accumulates contents and the separators between them.
struct Builder {
    parts: Vec<Doc>,
    start: Option<Gap>,
    gap: Gap,
    prev_word: bool,
}

impl Builder {
    fn push(&mut self, doc: Doc, word: bool) {
        let gap = std::mem::replace(&mut self.gap, Gap::None);
        if self.start.is_none() {
            self.start = Some(gap);
        } else {
            self.parts.push(separator(gap, self.prev_word, word));
        }
        self.parts.push(doc);
        self.prev_word = word;
    }

    /// Records the whitespace run `run` (spaces, tabs, line breaks).
    fn whitespace(&mut self, run: &str) {
        self.gap = if run.contains(['\n', '\r']) {
            Gap::Newline
        } else {
            Gap::Spaces(run.replace('\t', " "))
        };
    }

    fn finish(self) -> Children {
        let (start, end) = match self.start {
            Some(start) => (start, self.gap),
            None => (self.gap, Gap::None),
        };
        Children {
            start: edge(start),
            parts: self.parts,
            end: edge(end),
        }
    }
}

/// The separator between two contents (`a_word`/`b_word`: is it a text word?).
fn separator(gap: Gap, a_word: bool, b_word: bool) -> Doc {
    let words = a_word && b_word;
    match gap {
        Gap::Spaces(s) if words && s == " " => line(),
        Gap::Spaces(s) => text(s),
        Gap::Newline if words => line(),
        Gap::Newline => hardline(),
        Gap::None if !a_word && !b_word => hardline(),
        Gap::None => softline(),
    }
}

fn edge(gap: Gap) -> Edge {
    match gap {
        Gap::Spaces(s) => Edge::Glue(s),
        Gap::None | Gap::Newline => Edge::Break,
    }
}

/// JSX whitespace (React trims and joins on these only).
fn is_jsx_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r')
}

impl Printer<'_> {
    /// The children of an element, as fill parts plus what touches the tags.
    pub(super) fn jsx_children(&mut self, children: &[JsxChild]) -> Children {
        let mut b = Builder {
            parts: vec![],
            start: None,
            gap: Gap::None,
            prev_word: false,
        };
        for child in children {
            if let JsxChild::Text { span, .. } = child {
                text_pieces(slice(self.src, *span), &mut b);
                continue;
            }
            let doc = self.jsx_child_node(child);
            b.push(doc, false);
        }
        b.finish()
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
                cat!["{", crate::doc::concat(docs), "}"]
            }
            JsxChild::Spread { expr, span } => self.jsx_spread(expr, span.hi),
            JsxChild::Text { .. } => nil(),
        }
    }
}

/// Splits raw text into words and the whitespace between them.
fn text_pieces(raw: &str, b: &mut Builder) {
    let mut rest = raw;
    while !rest.is_empty() {
        let space_len = rest.len() - rest.trim_start_matches(is_jsx_space).len();
        if space_len > 0 {
            b.whitespace(&rest[..space_len]);
            rest = &rest[space_len..];
            continue;
        }
        let word_len = rest.find(is_jsx_space).unwrap_or(rest.len());
        b.push(text(&rest[..word_len]), true);
        rest = &rest[word_len..];
    }
}
