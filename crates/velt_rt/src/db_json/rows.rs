//! Result rows as JSON text, which std decodes with the compile-time generated
//! `JSON.parse<T[]>` / `JSON.parse<T>`: `[{"id":1,"name":"a"},...]`.
//!
//! Keys are the column names verbatim (escaped once per query, not per row), so a column maps
//! to the field of the same name; `AS` renames. Encoding: integers as exact i64 digits (decoded
//! losslessly by `read_i64`, even beyond 2^53); floats in their shortest round-trip form
//! (ryu, integral values as integers), with NaN and ±Infinity as `null`; text escaped like
//! `JSON.stringify` (invalid UTF-8 becomes U+FFFD); blobs as arrays of byte values (a `u8[]`).

use crate::json::escape::push_json_string;

/// 2^53: every integer below it in magnitude is exactly representable as an `f64`.
const MAX_EXACT_INT: f64 = 9_007_199_254_740_992.0;

/// Builds the JSON text of a result set, row by row. Call [`begin_row`](Self::begin_row), then
/// one value method per column in column order, then [`end_row`](Self::end_row).
pub struct RowWriter {
    buf: Vec<u8>,
    /// Every column's `"name":` prefix, concatenated.
    keys: Vec<u8>,
    /// End offset of each column's prefix in `keys`.
    key_ends: Vec<usize>,
    col: usize,
    rows: usize,
}

impl RowWriter {
    /// A writer for rows with these column names.
    pub fn new<'n>(columns: impl IntoIterator<Item = &'n str>) -> RowWriter {
        let mut keys = Vec::new();
        let mut key_ends = Vec::new();
        for name in columns {
            push_json_string(&mut keys, name.as_bytes());
            keys.push(b':');
            key_ends.push(keys.len());
        }
        RowWriter {
            buf: Vec::with_capacity(64),
            keys,
            key_ends,
            col: 0,
            rows: 0,
        }
    }

    /// Number of columns.
    pub fn columns(&self) -> usize {
        self.key_ends.len()
    }

    /// Number of rows written so far.
    pub fn rows(&self) -> usize {
        self.rows
    }

    /// Start the next row (a `,` separates it from the previous one).
    pub fn begin_row(&mut self) {
        if self.rows > 0 {
            self.buf.push(b',');
        }
        self.buf.push(b'{');
        self.col = 0;
    }

    /// Finish the current row.
    pub fn end_row(&mut self) {
        self.buf.push(b'}');
        self.rows += 1;
    }

    /// Write the current column's key and advance to the next column.
    fn key(&mut self) {
        let start = match self.col {
            0 => 0,
            c => {
                self.buf.push(b',');
                self.key_ends[c - 1]
            }
        };
        self.buf
            .extend_from_slice(&self.keys[start..self.key_ends[self.col]]);
        self.col += 1;
    }

    /// `null`.
    pub fn null(&mut self) {
        self.key();
        self.buf.extend_from_slice(b"null");
    }

    /// `true` / `false`.
    pub fn bool(&mut self, v: bool) {
        self.key();
        self.buf
            .extend_from_slice(if v { b"true" } else { b"false" });
    }

    /// An exact integer.
    pub fn int(&mut self, v: i64) {
        self.key();
        crate::fmt::push_i64(&mut self.buf, v);
    }

    /// A float (`null` if not finite: JSON has no NaN or Infinity). Only the decoder reads this
    /// text, so it need not look like JavaScript's: an integral value within ±2^53 is written
    /// as an integer (it still decodes into an integer field), anything else as ryu's shortest
    /// round-trip form as is (`1.5`, `1e-7`), skipping `push_f64`'s JS-style reshaping.
    pub fn float(&mut self, v: f64) {
        self.key();
        if !v.is_finite() {
            self.buf.extend_from_slice(b"null");
        } else if v.fract() == 0.0 && v.abs() < MAX_EXACT_INT {
            crate::fmt::push_i64(&mut self.buf, v as i64);
        } else {
            self.buf
                .extend_from_slice(ryu::Buffer::new().format_finite(v).as_bytes());
        }
    }

    /// A string; bytes that are not valid UTF-8 become U+FFFD.
    pub fn text(&mut self, v: &[u8]) {
        self.key();
        match std::str::from_utf8(v) {
            Ok(s) => push_json_string(&mut self.buf, s.as_bytes()),
            Err(_) => push_json_string(&mut self.buf, String::from_utf8_lossy(v).as_bytes()),
        }
    }

    /// Write the next column's key and return the buffer, to which the caller appends exactly
    /// one JSON value (arrays and nested JSON that the typed methods cannot express).
    pub fn raw_value(&mut self) -> &mut Vec<u8> {
        self.key();
        &mut self.buf
    }

    /// A byte array, as a JSON array of numbers.
    pub fn bytes(&mut self, v: &[u8]) {
        self.key();
        push_byte_array(&mut self.buf, v);
    }

    /// Every row as a JSON array: `[{...},{...}]` (`[]` for none).
    pub fn into_array(self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.buf.len() + 2);
        out.push(b'[');
        out.extend_from_slice(&self.buf);
        out.push(b']');
        out
    }

    /// The rows written, without the enclosing array: the object `{...}` of a single row, or
    /// empty if no row was written.
    pub fn into_rows(self) -> Vec<u8> {
        self.buf
    }
}

/// Append `v` as a JSON array of byte values (`[0,255]`: how a `u8[]` decodes).
pub fn push_byte_array(out: &mut Vec<u8>, v: &[u8]) {
    out.reserve(v.len() * 4 + 2);
    out.push(b'[');
    for (i, &b) in v.iter().enumerate() {
        if i > 0 {
            out.push(b',');
        }
        crate::fmt::push_u64(out, b as u64);
    }
    out.push(b']');
}
