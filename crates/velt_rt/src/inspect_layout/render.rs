//! Writes a parsed value (`parse.rs`) the way node's `util.inspect` lays it out.

use super::group::{self, Columns};
use super::parse::{Container, Seg, Value};
use super::BREAK_LENGTH;
use std::ops::Range;

/// Buffers reused while writing (and between printed values).
#[derive(Default)]
pub(super) struct Work {
    /// Entry ranges of the containers being written, innermost last.
    ranges: Vec<Range<usize>>,
    /// Entries of broken containers moved out of the way, innermost last.
    moved: Vec<u8>,
    columns: Columns,
}

/// Writes `value` (parsed from `src`) laid out.
pub(super) struct Render<'a> {
    pub src: &'a [u8],
    pub value: &'a Value,
}

impl Render<'_> {
    /// An entry (a range of segments) whose values are printed at indentation `indent`.
    pub(super) fn entry(&self, segs: Range<usize>, indent: usize, w: &mut Work, out: &mut Vec<u8>) {
        for seg in &self.value.segs[segs] {
            match seg {
                Seg::Text(t) => out.extend_from_slice(&self.src[t.clone()]),
                Seg::Str(q) => render_string(&self.src[q.clone()], indent, out),
                Seg::Node(n) => self.container(&self.value.nodes[*n], indent, w, out),
            }
        }
    }

    /// Node's `reduceToSingleString`: `{ a, b }` when the entries fit on one line, else one
    /// entry (or one row of grouped array entries) per line. The entries are written on one
    /// line first (their own layout does not depend on this choice), and moved only when the
    /// container breaks.
    fn container(&self, c: &Container, indent: usize, w: &mut Work, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.src[c.open.clone()]);
        let body = out.len();
        out.push(b' ');
        let base = w.ranges.len();
        for (i, e) in self.value.entries[c.entries.clone()].iter().enumerate() {
            if i > 0 {
                out.extend_from_slice(b", ");
            }
            let start = out.len();
            self.entry(e.clone(), indent + 2, w, out);
            w.ranges.push(start - body..out.len() - body);
        }
        let ranges = &w.ranges[base..];
        let entries = &self.value.entries[c.entries.clone()];
        // Node groups an array's entries without its `... n more items`, which follows the rows.
        let more = c.close == b']' && entries.last().is_some_and(|e| self.is_more(e.clone()));
        let shown = &ranges[..ranges.len() - more as usize];
        let grouped = c.close == b']'
            && ranges.len() > 6
            && w.columns.plan(&out[body..], shown, ranges.len(), indent);
        if !grouped {
            let start = ranges.len() + indent + c.open_len + 10;
            let text = &out[body..];
            if below_break_length(text, ranges, start) && !text.contains(&b'\n') {
                out.extend_from_slice(&[b' ', c.close]);
                w.ranges.truncate(base);
                return;
            }
        }
        let moved = w.moved.len();
        w.moved.extend_from_slice(&out[body..]);
        out.truncate(body);
        let text = &w.moved[moved..];
        let pad = indent + 2;
        let newline = |out: &mut Vec<u8>, n: usize| {
            out.push(b'\n');
            out.resize(out.len() + n, b' ');
        };
        newline(out, pad);
        if grouped {
            let numbers = entries[..shown.len()]
                .iter()
                .all(|e| self.is_number(e.clone()));
            let mut separator = b",\n".to_vec();
            separator.resize(separator.len() + pad, b' ');
            w.columns.write(out, text, shown, numbers, &separator);
            if let Some(r) = ranges.get(shown.len()) {
                out.extend_from_slice(&separator);
                out.extend_from_slice(&text[r.clone()]);
            }
        } else {
            for (i, r) in ranges.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                    newline(out, pad);
                }
                out.extend_from_slice(&text[r.clone()]);
            }
        }
        newline(out, indent);
        out.push(c.close);
        w.moved.truncate(moved);
        w.ranges.truncate(base);
    }

    /// Is the entry node's `... n more items` (the glue's only unquoted text starting so)?
    fn is_more(&self, segs: Range<usize>) -> bool {
        matches!(&self.value.segs[segs], [Seg::Text(t)] if self.src[t.clone()].starts_with(b"... "))
    }

    /// Is the entry a number (`typeof value[i] === 'number'` for node's column alignment)?
    fn is_number(&self, segs: Range<usize>) -> bool {
        let [Seg::Text(t)] = &self.value.segs[segs] else {
            return false;
        };
        let t = &self.src[t.clone()];
        let t = t.strip_prefix(b"-").unwrap_or(t);
        let t = t.strip_suffix(b"n").unwrap_or(t);
        t == b"NaN"
            || t == b"Infinity"
            || (t.first().is_some_and(u8::is_ascii_digit)
                && t.iter()
                    .all(|&b| b.is_ascii_digit() || matches!(b, b'.' | b'e' | b'+' | b'-')))
    }
}

/// Node's `isBelowBreakLength` for the entries at `ranges` of `text`.
fn below_break_length(text: &[u8], ranges: &[Range<usize>], start: usize) -> bool {
    let mut total = ranges.len() + start;
    if total + ranges.len() > BREAK_LENGTH {
        return false;
    }
    for r in ranges {
        total += group::units(&text[r.clone()]);
        if total > BREAK_LENGTH {
            return false;
        }
    }
    true
}

/// A quoted string printed at indentation `indent`: node's `formatPrimitive` splits a string
/// longer than 16 units and than the room left on the line after each line break, each piece
/// quoted on its own, joined by ` +` and a new line.
fn render_string(quoted: &[u8], indent: usize, out: &mut Vec<u8>) {
    if !quoted.windows(2).any(|w| w == b"\\n") {
        return out.extend_from_slice(quoted);
    }
    let raw = crate::inspect::unescape_inspect_string(quoted);
    let len = group::units(&raw);
    if len <= 16 || len + indent + 4 <= BREAK_LENGTH || !raw.contains(&b'\n') {
        return out.extend_from_slice(quoted);
    }
    let mut separator = b" +\n".to_vec();
    separator.resize(separator.len() + indent + 2, b' ');
    for (i, line) in raw.split_inclusive(|&b| b == b'\n').enumerate() {
        if i > 0 {
            out.extend_from_slice(&separator);
        }
        crate::inspect::push_inspect_string(out, line);
    }
}
