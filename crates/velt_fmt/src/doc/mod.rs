//! Wadler/prettier-style document algebra: the printer builds a [`Doc`] tree of text, line breaks
//! and groups, and [`render`] lays it out within the target width.
//!
//! Break propagation happens at construction time: every node caches whether it contains a forced
//! break (hard line / break parent), and a [`group`] around such content is born broken — the same
//! result as prettier's `propagateBreaks` pass, without a second traversal. Children are shared
//! (`Rc`) so the alternative layouts of a [`conditional`] group can reuse the same sub-documents.

mod render;

pub(crate) use render::render;

use std::rc::Rc;

/// A document node with its cached "contains a forced break" flag.
#[derive(Clone, Debug)]
pub(crate) struct Doc(Rc<Inner>);

#[derive(Debug)]
struct Inner {
    node: Node,
    breaks: bool,
}

/// How a [`Node::Line`] renders when its group is flat.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LineKind {
    /// A space when flat.
    Space,
    /// Nothing when flat.
    Soft,
    /// Always a newline.
    Hard,
}

#[derive(Debug)]
pub(crate) enum Node {
    Nil,
    /// Literal text. May contain newlines only for verbatim content (template literals, block
    /// comments), which is never re-indented.
    Text(String),
    Concat(Vec<Doc>),
    Indent(Doc),
    /// Lays out its contents flat if they fit, else broken. `broken` forces the latter.
    Group {
        contents: Doc,
        broken: bool,
    },
    /// Tries each layout in order (the first flat, the others as they are) and keeps the first
    /// that fits; the last is the fallback. Does not propagate breaks to enclosing groups.
    Conditional(Vec<Doc>),
    /// Alternating contents and separators, laid out like a paragraph: each separator breaks
    /// only if the content after it does not fit on the line (prettier's `fill`).
    Fill(Vec<Doc>),
    Line(LineKind),
    /// `broken` if the enclosing group is broken, else `flat`.
    IfBreak {
        broken: Doc,
        flat: Doc,
    },
    /// Deferred to just before the next newline (end-of-line comments).
    LineSuffix(Doc),
    /// Forces the enclosing groups to break; prints nothing.
    BreakParent,
}

impl Doc {
    fn new(node: Node, breaks: bool) -> Doc {
        Doc(Rc::new(Inner { node, breaks }))
    }

    pub(crate) fn node(&self) -> &Node {
        &self.0.node
    }

    /// Whether this document contains a forced break (prettier's `willBreak`).
    pub(crate) fn breaks(&self) -> bool {
        self.0.breaks
    }

    /// Is this the empty document?
    pub(crate) fn is_nil(&self) -> bool {
        matches!(self.0.node, Node::Nil)
    }
}

impl From<&str> for Doc {
    fn from(s: &str) -> Doc {
        text(s)
    }
}

impl From<String> for Doc {
    fn from(s: String) -> Doc {
        text(s)
    }
}

/// The empty document.
pub(crate) fn nil() -> Doc {
    Doc::new(Node::Nil, false)
}

/// Literal text.
pub(crate) fn text(s: impl Into<String>) -> Doc {
    let s = s.into();
    if s.is_empty() {
        return nil();
    }
    Doc::new(Node::Text(s), false)
}

/// Concatenation; empty parts are dropped.
pub(crate) fn concat(parts: Vec<Doc>) -> Doc {
    let parts: Vec<Doc> = parts.into_iter().filter(|d| !d.is_nil()).collect();
    match parts.len() {
        0 => nil(),
        1 => parts.into_iter().next().unwrap_or_else(nil),
        _ => {
            let breaks = parts.iter().any(Doc::breaks);
            Doc::new(Node::Concat(parts), breaks)
        }
    }
}

/// Builds a [`concat`] from anything convertible to a document.
macro_rules! cat {
    ($($part:expr),* $(,)?) => {
        $crate::doc::concat(vec![$($crate::doc::Doc::from($part)),*])
    };
}
pub(crate) use cat;

impl From<&Doc> for Doc {
    fn from(d: &Doc) -> Doc {
        d.clone()
    }
}

/// Increases the indentation of the lines inside by one level.
pub(crate) fn indent(d: Doc) -> Doc {
    let breaks = d.breaks();
    Doc::new(Node::Indent(d), breaks)
}

/// A group: flat if it fits, broken otherwise (always broken if it contains a forced break).
pub(crate) fn group(d: Doc) -> Doc {
    let broken = d.breaks();
    Doc::new(
        Node::Group {
            contents: d,
            broken,
        },
        broken,
    )
}

/// A group that is always broken.
pub(crate) fn group_broken(d: Doc) -> Doc {
    Doc::new(
        Node::Group {
            contents: d,
            broken: true,
        },
        true,
    )
}

/// `d` as a broken group: reuses the contents of a group, wraps anything else.
pub(crate) fn expanded(d: &Doc) -> Doc {
    match d.node() {
        Node::Group { contents, .. } => group_broken(contents.clone()),
        _ => group_broken(d.clone()),
    }
}

/// Alternative layouts, tried in order (see [`Node::Conditional`]).
pub(crate) fn conditional(states: Vec<Doc>) -> Doc {
    Doc::new(Node::Conditional(states), false)
}

/// Paragraph layout of `parts` = `[content, separator, content, separator, …, content]`
/// (see [`Node::Fill`]).
pub(crate) fn fill(parts: Vec<Doc>) -> Doc {
    let breaks = parts.iter().any(Doc::breaks);
    Doc::new(Node::Fill(parts), breaks)
}

/// A space, or a newline when the group breaks.
pub(crate) fn line() -> Doc {
    Doc::new(Node::Line(LineKind::Space), false)
}

/// Nothing, or a newline when the group breaks.
pub(crate) fn softline() -> Doc {
    Doc::new(Node::Line(LineKind::Soft), false)
}

/// A newline, always.
pub(crate) fn hardline() -> Doc {
    Doc::new(Node::Line(LineKind::Hard), true)
}

/// `broken` inside a broken group, `flat` otherwise.
pub(crate) fn if_break(broken: impl Into<Doc>, flat: impl Into<Doc>) -> Doc {
    let (broken, flat) = (broken.into(), flat.into());
    let breaks = broken.breaks() || flat.breaks();
    Doc::new(Node::IfBreak { broken, flat }, breaks)
}

/// Content printed just before the next newline.
pub(crate) fn line_suffix(d: Doc) -> Doc {
    let breaks = d.breaks();
    Doc::new(Node::LineSuffix(d), breaks)
}

/// Forces the enclosing groups to break.
pub(crate) fn break_parent() -> Doc {
    Doc::new(Node::BreakParent, true)
}

/// `docs` separated by `sep`.
pub(crate) fn join(sep: &Doc, docs: Vec<Doc>) -> Doc {
    let mut parts = Vec::with_capacity(docs.len() * 2);
    for (i, d) in docs.into_iter().enumerate() {
        if i > 0 {
            parts.push(sep.clone());
        }
        parts.push(d);
    }
    concat(parts)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(items: &[&str]) -> Doc {
        let docs = items.iter().map(|s| text(*s)).collect();
        group(cat![
            "f(",
            indent(cat![softline(), join(&cat![",", line()], docs)]),
            if_break(",", ""),
            softline(),
            ")"
        ])
    }

    #[test]
    fn group_stays_flat_when_it_fits() {
        assert_eq!(render(&list(&["a", "b"]), 80), "f(a, b)");
    }

    #[test]
    fn group_breaks_with_trailing_comma() {
        assert_eq!(
            render(&list(&["aaaa", "bbbb"]), 8),
            "f(\n  aaaa,\n  bbbb,\n)"
        );
    }

    #[test]
    fn hard_line_propagates() {
        let d = group(cat!["a", line(), "b", hardline(), "c"]);
        assert!(d.breaks());
        assert_eq!(render(&d, 80), "a\nb\nc");
    }

    #[test]
    fn line_suffix_moves_before_newline() {
        let d = cat!["a", line_suffix(text(" // c")), ",", hardline(), "b"];
        assert_eq!(render(&d, 80), "a, // c\nb");
    }

    #[test]
    fn fill_breaks_only_where_needed() {
        let words = ["aaa", "bbb", "ccc", "ddd"].map(text);
        let mut parts = vec![];
        for (i, w) in words.into_iter().enumerate() {
            if i > 0 {
                parts.push(line());
            }
            parts.push(w);
        }
        assert_eq!(render(&group(fill(parts.clone())), 80), "aaa bbb ccc ddd");
        assert_eq!(render(&group(fill(parts)), 8), "aaa bbb\nccc ddd");
    }

    #[test]
    fn fill_puts_broken_content_on_its_own_line() {
        let block = group_broken(cat!["{", indent(cat![hardline(), "x"]), hardline(), "}"]);
        let d = group(fill(vec![text("a"), line(), block, line(), text("b")]));
        assert_eq!(render(&d, 80), "a\n{\n  x\n}\nb");
    }

    #[test]
    fn conditional_picks_first_fitting_state() {
        let flat = text("xxxxxxxxxx");
        let alt = cat!["x", hardline(), "y"];
        assert_eq!(render(&conditional(vec![flat, alt]), 5), "x\ny");
    }
}
