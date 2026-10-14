//! Result rows in a binary form that the JSON pull reader walks directly (#33; the reader is
//! `rows_reader.rs`), so a database driver's rows reach the compile-time generated
//! `JSON.parse<T[]>` decoder without becoming JSON text: no escaping and number formatting on
//! the way out, no scanning on the way in.
//!
//! Layout: a header `single: u8, columns: u16, (name_len: u32, name bytes)*`, then the rows,
//! each one cell per column. A cell is a tag byte and its payload: [`NULL`], [`FALSE`],
//! [`TRUE`], [`INT`] + `i64`, [`FLOAT`] + `f64`, [`TEXT`] or [`ASCII`] + `u32` length + UTF-8,
//! [`BLOB`] + `u32` length + bytes (integers little-endian). Text that is all ASCII says so
//! (the `ASCII` tag, [`ASCII_NAME`] in a name's length), because the writer looks at every
//! byte anyway and the reader then need not: such a string's UTF-16 length is its byte length.
//! To the decoder this reads as the JSON `db_json::rows` writes: `[{"col":value,...},...]`, or
//! with `single` the one row's object.
//!
//! Hand-off: std still gets a string from the driver and passes it to `JSON.parse`. A string
//! cannot hold these bytes (they are not text), so the driver parks the buffer in a
//! thread-local and returns [`hand_off`]'s marker string, whose bytes are a static of this
//! module; `velt_rt_json_reader_new_with` recognizes the marker by address ([`take_pending`]),
//! which no other string can have, and reads the parked buffer. std calls `JSON.parse` right
//! after the query, on the same thread, without awaiting in between.

use super::escape::push_json_string;
use crate::str::VeltStr;
use std::cell::Cell;

pub const NULL: u8 = 0;
pub const FALSE: u8 = 1;
pub const TRUE: u8 = 2;
pub const INT: u8 = 3;
pub const FLOAT: u8 = 4;
pub const TEXT: u8 = 5;
pub const BLOB: u8 = 6;
/// [`TEXT`] that is all ASCII.
pub const ASCII: u8 = 7;
/// Set in a column name's length when the name is all ASCII.
pub const ASCII_NAME: u32 = 0x8000_0000;

/// 2^53: every integer below it in magnitude is exactly representable as an `f64`.
const MAX_EXACT_INT: f64 = 9_007_199_254_740_992.0;

/// What the marker string says, should it ever be read as text (it is not JSON).
static MARKER: [u8; 6] = *b"<rows>";

thread_local! {
    static PENDING: Cell<Option<Vec<u8>>> = const { Cell::new(None) };
}

/// Park `rows` (from [`BinRows::finish`]) for the reader this thread creates next from the
/// returned marker string.
pub fn hand_off(rows: Vec<u8>) -> VeltStr {
    PENDING.with(|p| p.set(Some(rows)));
    VeltStr::from_static(&MARKER)
}

/// The parked rows, if `src` is the marker string's bytes.
pub fn take_pending(src: &[u8]) -> Option<Vec<u8>> {
    if !std::ptr::eq(src.as_ptr(), MARKER.as_ptr()) {
        return None;
    }
    match PENDING.with(|p| p.take()) {
        Some(rows) => Some(rows),
        None => crate::panic::fatal("ICE: JSON.parse of a query result with no rows parked"),
    }
}

/// Builds a result set: one value method per column, in column order, for every row.
pub struct BinRows {
    buf: Vec<u8>,
    rows: usize,
}

impl BinRows {
    /// A writer for rows with these column names; `single` for one row read as an object
    /// rather than an array of rows.
    pub fn new<'n>(columns: impl IntoIterator<Item = &'n str>, single: bool) -> BinRows {
        let mut buf = Vec::with_capacity(256);
        buf.push(single as u8);
        buf.extend_from_slice(&[0, 0]);
        let mut count: u16 = 0;
        for name in columns {
            // A name is as long as the SQL allows (SQLite's limit is at most 2^30 bytes).
            if name.len() >= ASCII_NAME as usize {
                crate::panic::fatal("database column name too long");
            }
            let ascii = if name.is_ascii() { ASCII_NAME } else { 0 };
            buf.extend_from_slice(&(name.len() as u32 | ascii).to_le_bytes());
            buf.extend_from_slice(name.as_bytes());
            count += 1;
        }
        buf[1..3].copy_from_slice(&count.to_le_bytes());
        BinRows { buf, rows: 0 }
    }

    /// The row whose cells were just written is complete.
    pub fn end_row(&mut self) {
        self.rows += 1;
    }

    /// Number of rows written so far.
    pub fn rows(&self) -> usize {
        self.rows
    }

    pub fn null(&mut self) {
        self.buf.push(NULL);
    }

    pub fn bool(&mut self, v: bool) {
        self.buf.push(if v { TRUE } else { FALSE });
    }

    pub fn int(&mut self, v: i64) {
        self.buf.push(INT);
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    /// A float; NaN and ±Infinity are `null`, and -0 is 0, as in the JSON text the rows
    /// stand for.
    pub fn float(&mut self, v: f64) {
        if !v.is_finite() {
            return self.null();
        }
        let v = if v == 0.0 { 0.0 } else { v };
        self.buf.push(FLOAT);
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    /// A string; bytes that are not valid UTF-8 become U+FFFD.
    pub fn text(&mut self, v: &[u8]) {
        if v.is_ascii() {
            return self.sized(ASCII, v);
        }
        match std::str::from_utf8(v) {
            Ok(_) => self.sized(TEXT, v),
            Err(_) => self.sized(TEXT, String::from_utf8_lossy(v).as_bytes()),
        }
    }

    /// A byte array (a `u8[]`).
    pub fn bytes(&mut self, v: &[u8]) {
        self.sized(BLOB, v);
    }

    fn sized(&mut self, tag: u8, v: &[u8]) {
        if v.len() > u32::MAX as usize {
            crate::panic::fatal("database value too long");
        }
        self.buf.reserve(v.len() + 5);
        self.buf.push(tag);
        self.buf.extend_from_slice(&(v.len() as u32).to_le_bytes());
        self.buf.extend_from_slice(v);
    }

    /// The finished buffer.
    pub fn finish(self) -> Vec<u8> {
        self.buf
    }
}

/// One decoded cell; text and blobs as ranges of the buffer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BinCell {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    /// The range and whether the text is all ASCII.
    Text(usize, usize, bool),
    Blob(usize, usize),
}

/// The cell at `pos` and where the next one starts; `None` if the buffer ends early or the
/// tag is unknown (never for a buffer [`BinRows`] wrote).
#[inline]
pub fn cell_at(buf: &[u8], pos: usize) -> Option<(BinCell, usize)> {
    let at = pos + 1;
    Some(match *buf.get(pos)? {
        NULL => (BinCell::Null, at),
        FALSE => (BinCell::Bool(false), at),
        TRUE => (BinCell::Bool(true), at),
        INT => {
            let b = buf.get(at..at + 8)?;
            (BinCell::Int(i64::from_le_bytes(b.try_into().ok()?)), at + 8)
        }
        FLOAT => {
            let b = buf.get(at..at + 8)?;
            (
                BinCell::Float(f64::from_le_bytes(b.try_into().ok()?)),
                at + 8,
            )
        }
        tag @ (TEXT | ASCII | BLOB) => {
            let b = buf.get(at..at + 4)?;
            let start = at + 4;
            let end = start.checked_add(u32::from_le_bytes(b.try_into().ok()?) as usize)?;
            if end > buf.len() {
                return None;
            }
            if tag == BLOB {
                (BinCell::Blob(start, end), end)
            } else {
                (BinCell::Text(start, end, tag == ASCII), end)
            }
        }
        _ => return None,
    })
}

/// Where the first column's name starts in the header (at its length prefix).
pub const NAMES: usize = 3;

/// The header of a buffer: whether it is a single row, the number of columns, and where the
/// rows start. `None` unless every name is in bounds.
pub fn header_end(buf: &[u8]) -> Option<(bool, usize, usize)> {
    let single = *buf.first()? != 0;
    let count = u16::from_le_bytes(buf.get(1..3)?.try_into().ok()?) as usize;
    let mut pos = NAMES;
    for _ in 0..count {
        let len = u32::from_le_bytes(buf.get(pos..pos + 4)?.try_into().ok()?);
        pos += 4 + (len & !ASCII_NAME) as usize;
        if pos > buf.len() {
            return None;
        }
    }
    Some((single, count, pos))
}

/// The range of the column name whose length prefix is at `pos`, and whether the name is all
/// ASCII, in a buffer whose header [`header_end`] accepted; the next name's prefix is at the
/// range's end.
#[inline]
pub fn name_at(buf: &[u8], pos: usize) -> (usize, usize, bool) {
    let len = u32::from_le_bytes([buf[pos], buf[pos + 1], buf[pos + 2], buf[pos + 3]]);
    let end = pos + 4 + (len & !ASCII_NAME) as usize;
    (pos + 4, end, len & ASCII_NAME != 0)
}

/// Append one cell as a JSON value: a float in its shortest form that reads back the same, a
/// blob as an array of byte values.
pub fn push_cell_json(out: &mut Vec<u8>, buf: &[u8], cell: BinCell) {
    match cell {
        BinCell::Null => out.extend_from_slice(b"null"),
        BinCell::Bool(v) => out.extend_from_slice(if v { b"true" } else { b"false" }),
        BinCell::Int(v) => crate::fmt::push_i64(out, v),
        BinCell::Float(v) if v.fract() == 0.0 && v.abs() < MAX_EXACT_INT => {
            crate::fmt::push_i64(out, v as i64)
        }
        BinCell::Float(v) => crate::fmt::push_f64(out, v),
        BinCell::Text(start, end, _) => push_json_string(out, &buf[start..end]),
        BinCell::Blob(start, end) => {
            out.push(b'[');
            for (i, &b) in buf[start..end].iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                crate::fmt::push_u64(out, b as u64);
            }
            out.push(b']');
        }
    }
}

/// Append the row at `pos` as a JSON object; where the next row starts.
pub fn push_row_json(out: &mut Vec<u8>, buf: &[u8], mut pos: usize) -> Option<usize> {
    let (_, columns, _) = header_end(buf)?;
    let mut name = NAMES;
    out.push(b'{');
    for i in 0..columns {
        if i > 0 {
            out.push(b',');
        }
        let (start, end, _) = name_at(buf, name);
        name = end;
        push_json_string(out, &buf[start..end]);
        out.push(b':');
        let (cell, next) = cell_at(buf, pos)?;
        push_cell_json(out, buf, cell);
        pos = next;
    }
    out.push(b'}');
    Some(pos)
}

/// The whole buffer as the JSON text it stands for: `[{...},...]`, or a single row's object.
pub fn to_json(buf: &[u8]) -> Option<Vec<u8>> {
    let (single, _, mut pos) = header_end(buf)?;
    let mut out = Vec::with_capacity(buf.len() + buf.len() / 4);
    if single {
        push_row_json(&mut out, buf, pos)?;
        return Some(out);
    }
    out.push(b'[');
    while pos < buf.len() {
        if out.len() > 1 {
            out.push(b',');
        }
        pos = push_row_json(&mut out, buf, pos)?;
    }
    out.push(b']');
    Some(out)
}
