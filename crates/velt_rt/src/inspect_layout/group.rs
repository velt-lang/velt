//! Node's `groupArrayElements`: an array of more than six short entries prints them in aligned
//! columns, numbers right-aligned. Lengths are counted as node counts them: UTF-16 units for the
//! line-length checks, display columns (`getStringWidth`) for the column widths.

use super::BREAK_LENGTH;
use std::ops::Range;

/// Node's `ctx.compact` (3): limits the columns to `compact * 4`.
const COMPACT: usize = 3;

/// Room for `, ` after an entry.
const SEPARATOR_SPACE: usize = 2;

/// How the entries of an array are grouped into rows (buffers reused between arrays).
#[derive(Default)]
pub(super) struct Columns {
    count: usize,
    /// Display width of each entry.
    data_len: Vec<usize>,
    /// Width of each column (its widest entry).
    widest: Vec<usize>,
}

impl Columns {
    /// Plan the columns for the entries at `ranges` of `text` (an array at indentation
    /// `indent`), of `entries` in all (one more when `... n more items` follows them, which
    /// node counts only in its average); false when grouping does not pay off.
    pub(super) fn plan(
        &mut self,
        text: &[u8],
        ranges: &[Range<usize>],
        entries: usize,
        indent: usize,
    ) -> bool {
        self.data_len.clear();
        self.data_len
            .extend(ranges.iter().map(|r| width(&text[r.clone()])));
        let total_length: usize = self.data_len.iter().map(|l| l + SEPARATOR_SPACE).sum();
        let max_length = self.data_len.iter().copied().max().unwrap_or(0);
        let actual_max = max_length + SEPARATOR_SPACE;
        if actual_max * 3 + indent >= BREAK_LENGTH
            || (total_length as f64 / actual_max as f64 <= 5.0 && max_length > 6)
        {
            return false;
        }
        let n = ranges.len();
        let average_bias = (actual_max as f64 - total_length as f64 / entries as f64).sqrt();
        let biased_max = (actual_max as f64 - 3.0 - average_bias).max(1.0);
        self.count = js_round((2.5 * biased_max * n as f64).sqrt() / biased_max)
            .min((BREAK_LENGTH.saturating_sub(indent) / actual_max) as f64)
            .min((COMPACT * 4) as f64)
            .min(15.0) as usize;
        if self.count <= 1 {
            return false;
        }
        let (count, data_len) = (self.count, &self.data_len);
        self.widest.clear();
        self.widest.extend((0..count).map(|i| {
            (i..n)
                .step_by(count)
                .map(|j| data_len[j])
                .max()
                .unwrap_or(0)
        }));
        true
    }

    /// Write the rows (as planned) of the entries at `ranges` of `text`, separated by
    /// `separator`.
    /// `numbers`: every entry is a number, right-aligned as node pads them (else left-aligned).
    pub(super) fn write(
        &self,
        out: &mut Vec<u8>,
        text: &[u8],
        ranges: &[Range<usize>],
        numbers: bool,
        separator: &[u8],
    ) {
        for (j, r) in ranges.iter().enumerate() {
            let column = j % self.count;
            let last_in_row = column + 1 == self.count || j + 1 == ranges.len();
            if j > 0 && column == 0 {
                out.extend_from_slice(separator);
            }
            let pad = self.widest[column].saturating_sub(self.data_len[j]);
            if numbers {
                out.resize(out.len() + pad, b' ');
            }
            out.extend_from_slice(&text[r.clone()]);
            if !last_in_row {
                out.extend_from_slice(b", ");
                if !numbers {
                    out.resize(out.len() + pad, b' ');
                }
            }
        }
    }
}

/// JavaScript's `Math.round` for the non-negative values here (halves round up).
fn js_round(x: f64) -> f64 {
    (x + 0.5).floor()
}

/// UTF-16 length of WTF-8 text (`String.prototype.length`).
pub(super) fn units(text: &[u8]) -> usize {
    if text.is_ascii() {
        return text.len();
    }
    text.iter()
        .map(|&b| match b {
            0x80..=0xbf => 0,
            0xf0..=0xff => 2,
            _ => 1,
        })
        .sum()
}

/// Display columns of `text`, as node's `getStringWidth`: control characters and combining
/// marks take none, East Asian wide characters two.
pub(super) fn width(text: &[u8]) -> usize {
    if text.is_ascii() {
        return text.iter().filter(|&&b| b >= 0x20 && b != 0x7f).count();
    }
    String::from_utf8_lossy(text)
        .chars()
        .map(|c| match c as u32 {
            c if zero_width(c) => 0,
            c if full_width(c) => 2,
            _ => 1,
        })
        .sum()
}

fn zero_width(c: u32) -> bool {
    c <= 0x1f
        || (0x7f..=0x9f).contains(&c)
        || (0x300..=0x36f).contains(&c)
        || (0x200b..=0x200f).contains(&c)
        || (0x20d0..=0x20ff).contains(&c)
        || (0xfe00..=0xfe0f).contains(&c)
        || (0xfe20..=0xfe2f).contains(&c)
        || (0xe0100..=0xe01ef).contains(&c)
}

fn full_width(c: u32) -> bool {
    c >= 0x1100
        && (c <= 0x115f
            || c == 0x2329
            || c == 0x232a
            || ((0x2e80..=0x3247).contains(&c) && c != 0x303f)
            || (0x3250..=0x4dbf).contains(&c)
            || (0x4e00..=0xa4c6).contains(&c)
            || (0xa960..=0xa97c).contains(&c)
            || (0xac00..=0xd7a3).contains(&c)
            || (0xf900..=0xfaff).contains(&c)
            || (0xfe10..=0xfe19).contains(&c)
            || (0xfe30..=0xfe6b).contains(&c)
            || (0xff01..=0xff60).contains(&c)
            || (0xffe0..=0xffe6).contains(&c)
            || (0x1b000..=0x1b001).contains(&c)
            || (0x1f200..=0x1f251).contains(&c)
            || (0x1f300..=0x1f64f).contains(&c)
            || (0x20000..=0x3fffd).contains(&c))
}
