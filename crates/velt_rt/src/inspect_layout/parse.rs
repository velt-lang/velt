//! Reads the one-line text the print glue wrote for a value back into its containers, so
//! `render.rs` can lay it out on several lines. The glue's text is regular: a container opens
//! with `{ ` or `[ ` (after a prefix such as `Name `, `Map(2) ` or `<ref *1> `), separates
//! entries with `, ` and closes with ` }` or ` ]`; strings are quoted with their quotes escaped,
//! and other bracketed atoms (`[Circular *1]`, `[Function (anonymous)]`) have no space after `[`.
//!
//! Pieces are byte ranges of the text kept in flat arrays ([`Value`]) that are reused from one
//! printed value to the next, so reading a value allocates nothing once they have grown.

use std::ops::Range;

/// One piece of an entry.
#[derive(Clone)]
pub(super) enum Seg {
    /// Text printed as is (keys, numbers, `: `, ` => `, atoms).
    Text(Range<usize>),
    /// A quoted string value (node splits a long one at its line breaks).
    Str(Range<usize>),
    /// A nested container: its index in [`Value::nodes`].
    Node(usize),
}

/// A container with at least one entry.
pub(super) struct Container {
    /// The prefix and opening brace (`<ref *1> Name {`).
    pub open: Range<usize>,
    /// Node's `braces[0].length + base.length`: the opening text without the space after a
    /// `<ref *N>` base.
    pub open_len: usize,
    /// `}` or `]`; an array (`]`) may group short entries into columns.
    pub close: u8,
    /// Its entries, as a range of [`Value::entries`].
    pub entries: Range<usize>,
}

/// A parsed value: the top-level entry is `segs[top]`.
#[derive(Default)]
pub(super) struct Value {
    /// Each entry's pieces are a range of this.
    pub segs: Vec<Seg>,
    pub nodes: Vec<Container>,
    /// Each container's entries (ranges of `segs`) are a range of this.
    pub entries: Vec<Range<usize>>,
    pub top: Range<usize>,
    /// Pieces of the entries being read, innermost last.
    pending: Vec<Seg>,
    /// Entries of the containers being read, innermost last.
    pending_entries: Vec<Range<usize>>,
}

impl Value {
    /// Parse a value's text into `self` (cleared first), keeping each of `atoms` (custom inspect
    /// text) as plain text; false if the text does not have the
    /// glue's shape (it is then printed as it is).
    pub(super) fn parse(&mut self, text: &[u8], atoms: &[Range<usize>]) -> bool {
        self.segs.clear();
        self.nodes.clear();
        self.entries.clear();
        self.pending.clear();
        self.pending_entries.clear();
        let mut p = Parser {
            s: text,
            atoms,
            i: 0,
            v: self,
        };
        match p.entry(None) {
            Some((top, End::Eof)) => {
                self.top = top;
                true
            }
            _ => false,
        }
    }
}

/// How an entry ended.
#[derive(PartialEq)]
enum End {
    Eof,
    Comma,
    Close,
}

struct Parser<'a> {
    /// Ranges of `s` kept as plain text (`velt_rt_strbuf_inspect_atom`).
    atoms: &'a [Range<usize>],
    s: &'a [u8],
    i: usize,
    v: &'a mut Value,
}

/// Bytes the scanner stops at; everything else is plain text.
const SPECIAL: [bool; 256] = {
    let mut t = [false; 256];
    let bytes = b" ,:<'\"`{[()";
    let mut k = 0;
    while k < bytes.len() {
        t[bytes[k] as usize] = true;
        k += 1;
    }
    t
};

impl Parser<'_> {
    /// One entry, up to `, ` or the closing ` }` / ` ]` of the container (`close`), or to the
    /// end of the text for the top-level value (`close == None`). Returns its pieces as a range
    /// of `segs`.
    fn entry(&mut self, close: Option<u8>) -> Option<(Range<usize>, End)> {
        let s = self.s;
        let base = self.v.pending.len();
        // Where the pending text starts, and where the current value starts in it.
        let (mut text, mut value) = (self.i, self.i);
        let mut parens = 0u32;
        let end = loop {
            while self.i < s.len() && !SPECIAL[s[self.i] as usize] {
                self.i += 1;
            }
            let i = self.i;
            if let Some(atom) = self.atoms.iter().find(|a| a.contains(&i)) {
                // Custom inspect text: plain text, whatever it looks like.
                self.i = atom.end;
                continue;
            }
            let Some(&c) = s.get(i) else {
                if close.is_some() || parens > 0 {
                    return None;
                }
                break End::Eof;
            };
            let next = s.get(i + 1).copied();
            let separates = parens == 0 && close.is_some();
            match c {
                b' ' if separates && next == close => {
                    self.i += 2;
                    break End::Close;
                }
                b' ' if s[i..].starts_with(b" => ") => {
                    self.i += 4;
                    value = self.i;
                }
                b',' if separates && next == Some(b' ') => {
                    self.i += 2;
                    break End::Comma;
                }
                b':' if next == Some(b' ') => {
                    self.i += 2;
                    value = self.i;
                }
                b'<' if s[i..].starts_with(b"<rejected> ") => {
                    self.i += 11;
                    value = self.i;
                }
                b'\'' | b'"' | b'`' => {
                    self.skip_quoted()?;
                    if !s[self.i..].starts_with(b": ") {
                        // A string value (a quoted key stays in the text).
                        self.push_text(text..i);
                        self.v.pending.push(Seg::Str(i..self.i));
                        (text, value) = (self.i, self.i);
                    }
                }
                b'{' | b'[' if next == Some(b' ') => {
                    self.push_text(text..value);
                    let node = self.container(value)?;
                    self.v.pending.push(Seg::Node(node));
                    (text, value) = (self.i, self.i);
                }
                b'[' if next != Some(b']') => self.skip_bracketed()?,
                b'(' => {
                    parens += 1;
                    self.i += 1;
                }
                b')' => {
                    parens = parens.checked_sub(1)?;
                    self.i += 1;
                }
                _ => self.i += 1,
            }
        };
        let text_end = match end {
            End::Eof => self.i,
            _ => self.i - 2,
        };
        self.push_text(text..text_end);
        if self.v.pending.len() == base {
            return None;
        }
        let start = self.v.segs.len();
        let pieces = self.v.pending.drain(base..);
        self.v.segs.extend(pieces);
        Some((start..self.v.segs.len(), end))
    }

    fn push_text(&mut self, r: Range<usize>) {
        if !r.is_empty() {
            self.v.pending.push(Seg::Text(r));
        }
    }

    /// A container whose prefix starts at `open` and whose brace is at `self.i`.
    fn container(&mut self, open: usize) -> Option<usize> {
        let brace = self.s[self.i];
        let open = open..self.i + 1;
        self.i += 2;
        let close = if brace == b'{' { b'}' } else { b']' };
        let base = self.v.pending_entries.len();
        loop {
            let (e, end) = self.entry(Some(close))?;
            self.v.pending_entries.push(e);
            if end == End::Close {
                break;
            }
        }
        let start = self.v.entries.len();
        let entries = self.v.pending_entries.drain(base..);
        self.v.entries.extend(entries);
        let has_base = self.s[open.clone()].starts_with(b"<ref *");
        // A custom inspect's `Name ` before the brace (`Headers {`): node formats the object
        // after it by itself, where only the brace counts.
        let brace_at = open.end - 1;
        let custom = self
            .atoms
            .iter()
            .any(|a| a.end == brace_at && a.start >= open.start);
        let open_len = match custom {
            true => 1,
            false => super::group::units(&self.s[open.clone()]) - has_base as usize,
        };
        self.v.nodes.push(Container {
            open,
            open_len,
            close,
            entries: start..self.v.entries.len(),
        });
        Some(self.v.nodes.len() - 1)
    }

    /// Skip the quoted string at `self.i` (a backslash escapes the next byte).
    fn skip_quoted(&mut self) -> Option<()> {
        let quote = self.s[self.i];
        let mut j = self.i + 1;
        loop {
            match *self.s.get(j)? {
                b'\\' => j += 2,
                c if c == quote => break,
                _ => j += 1,
            }
        }
        self.i = j + 1;
        Some(())
    }

    /// Skip the bracketed atom at `self.i` (`[Circular *1]`), nested brackets included.
    fn skip_bracketed(&mut self) -> Option<()> {
        let mut depth = 0usize;
        for (j, &c) in self.s[self.i..].iter().enumerate() {
            match c {
                b'[' => depth += 1,
                b']' => {
                    depth -= 1;
                    if depth == 0 {
                        self.i += j + 1;
                        return Some(());
                    }
                }
                _ => {}
            }
        }
        None
    }
}
