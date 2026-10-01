//! Sequences with comments: line-per-entry bodies ([`Printer::lines`]: statements, items, members,
//! match arms, enum variants — blank lines kept, at most one) and comma-separated delimited lists
//! ([`Printer::list`] + [`delimited`]: arguments, parameters, arrays, objects — trailing comma
//! when broken).
//!
//! An entry owns the comments that precede it (leading) and those that end its line (trailing,
//! kept at the end of the line). Comments after the last entry stay inside the container.

use super::Printer;
use crate::comments::Comment;
use crate::doc::{
    break_parent, cat, concat, group, group_broken, hardline, if_break, indent, join, line,
    line_suffix, softline, text, Doc,
};
use crate::source::blank_line_before;

/// Accumulates the parts of a line-per-entry body.
struct Lines {
    parts: Vec<Doc>,
    started: bool,
    /// The last part was an inline block comment: the next part continues its line.
    inline: bool,
}

impl Lines {
    fn separate(&mut self, blank: bool) {
        if !self.started {
            self.started = true;
        } else if self.inline {
            self.inline = false;
        } else {
            self.parts.push(hardline());
            if blank {
                self.parts.push(hardline());
            }
        }
    }

    fn comment(&mut self, src: &str, c: &Comment, forced_blank: bool) {
        self.separate(forced_blank || blank_line_before(src, c.lo));
        self.parts.push(text(c.text.clone()));
        self.inline = !c.needs_newline();
        if self.inline {
            self.parts.push(text(" "));
        }
    }
}

/// Entries of a delimited list, with the comments around them attached.
pub(super) struct ListDocs {
    pub(super) items: Vec<Doc>,
    /// Comments of an empty list.
    pub(super) dangling: Vec<Comment>,
    /// Some entry carries a comment (disables special layouts such as argument hugging).
    pub(super) has_comments: bool,
}

impl<'a> Printer<'a> {
    /// One entry per line; `forced_blank(prev, cur)` demands a blank line between two entries,
    /// otherwise blank lines from the source are kept (at most one).
    pub(super) fn lines<T>(
        &mut self,
        entries: &[T],
        end: u32,
        range: impl Fn(&T) -> (u32, u32),
        forced_blank: impl Fn(&T, &T) -> bool,
        print: impl Fn(&mut Self, &T) -> Doc,
    ) -> Doc {
        let src = self.src;
        let mut out = Lines {
            parts: vec![],
            started: false,
            inline: false,
        };
        for (i, entry) in entries.iter().enumerate() {
            let (lo, hi) = range(entry);
            let forced = i > 0 && forced_blank(&entries[i - 1], entry);
            let leading = self.comments.take_before(lo);
            for (k, c) in leading.iter().enumerate() {
                out.comment(src, c, forced && k == 0);
            }
            out.separate((forced && leading.is_empty()) || blank_line_before(src, lo));
            out.parts.push(print(self, entry));
            let next_lo = entries.get(i + 1).map_or(end, |n| range(n).0);
            let mut trailing = self.comments.take_before(hi);
            trailing.extend(self.comments.take_trailing(src, hi, next_lo, true));
            for c in trailing {
                out.parts.push(text(" "));
                out.parts.push(text(c.text));
            }
        }
        for c in self.comments.take_before(end) {
            out.comment(src, &c, false);
        }
        concat(out.parts)
    }

    /// Prints the entries of a delimited list ending before `end`, attaching comments.
    pub(super) fn list<T>(
        &mut self,
        entries: &[T],
        end: u32,
        range: impl Fn(&T) -> (u32, u32),
        print: impl Fn(&mut Self, &T) -> Doc,
    ) -> ListDocs {
        let src = self.src;
        let mut out = ListDocs {
            items: vec![],
            dangling: vec![],
            has_comments: false,
        };
        for (i, entry) in entries.iter().enumerate() {
            let (lo, hi) = range(entry);
            let leading = self.comments.take_before(lo);
            let doc = print(self, entry);
            let mut parts = vec![self.leading_doc(&leading), doc];
            let mut trailing = self.comments.take_before(hi);
            let last = i + 1 == entries.len();
            let next_lo = entries.get(i + 1).map_or(end, |n| range(n).0);
            trailing.extend(self.comments.take_trailing(src, hi, next_lo, last));
            let own_line = if last {
                self.comments.take_before(end)
            } else {
                vec![]
            };
            out.has_comments |= !leading.is_empty() || !trailing.is_empty() || !own_line.is_empty();
            parts.extend(trailing.iter().map(end_of_line_comment));
            parts.extend(own_line.iter().map(|c| {
                cat![
                    line_suffix(cat![hardline(), text(c.text.clone())]),
                    break_parent()
                ]
            }));
            out.items.push(concat(parts));
        }
        if entries.is_empty() {
            out.dangling = self.comments.take_before(end);
            out.has_comments = !out.dangling.is_empty();
        }
        out
    }
}

/// A comment that stays at the end of the line of the entry before it.
fn end_of_line_comment(c: &Comment) -> Doc {
    if c.is_block {
        cat![" ", text(c.text.clone())]
    } else {
        cat![line_suffix(text(format!(" {}", c.text))), break_parent()]
    }
}

/// `open entries close` as one group: flat (`(a, b)`, `{ a, b }` with `spaced`), or one entry per
/// line with a trailing comma.
pub(super) fn delimited(open: &str, list: ListDocs, close: &str, spaced: bool) -> Doc {
    delimited_with(open, list, close, spaced, false)
}

/// [`delimited`], optionally always broken.
pub(super) fn delimited_with(
    open: &str,
    list: ListDocs,
    close: &str,
    spaced: bool,
    force_break: bool,
) -> Doc {
    if list.items.is_empty() {
        return dangling(open, &list.dangling, close, spaced);
    }
    let edge = if spaced { line() } else { softline() };
    let contents = cat![
        open,
        indent(cat![edge.clone(), join(&cat![",", line()], list.items)]),
        if_break(",", ""),
        edge,
        close
    ];
    if force_break {
        group_broken(contents)
    } else {
        group(contents)
    }
}

/// An empty list, keeping the comments inside it (`{ /* c */ }` with `spaced`).
pub(super) fn dangling(open: &str, comments: &[Comment], close: &str, spaced: bool) -> Doc {
    if comments.is_empty() {
        return cat![open, close];
    }
    let texts = comments.iter().map(|c| text(c.text.clone())).collect();
    let forced = if comments.iter().any(Comment::needs_newline) {
        break_parent()
    } else {
        crate::doc::nil()
    };
    let edge = if spaced { line() } else { softline() };
    group(cat![
        open,
        indent(cat![edge.clone(), join(&line(), texts)]),
        forced,
        edge,
        close
    ])
}
