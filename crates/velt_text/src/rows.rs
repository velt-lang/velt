//! Result rows in a binary form that Velt's JSON pull reader walks directly (#33), so a
//! database driver's rows reach the compile-time generated `JSON.parse<T[]>` decoder without
//! becoming JSON text: no escaping and number formatting on the way out, no scanning on the way
//! in. A driver returns them as a `u8[]` and std decodes them with the same decoder as JSON text
//! (`Intrinsic::JsonParseRows`; native_abi.md "Rows").
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
//! The reader trusts what the header and tags say about text (UTF-8, ASCII): rows come from
//! [`BinRows`], which checks them. A buffer cut short reads as a mismatch, never out of bounds.

use crate::json_escape::push_json_string;
use crate::rows_cursor::Cell;
use crate::Out;

pub const NULL: u8 = 0;
pub const FALSE: u8 = 1;
pub const TRUE: u8 = 2;
pub const INT: u8 = 3;
pub const FLOAT: u8 = 4;
pub const TEXT: u8 = 5;
pub const BLOB: u8 = 6;
/// [`TEXT`] that is all ASCII.
pub const ASCII: u8 = 7;
/// A nested value (an array or object a database column holds, such as a Postgres array or
/// `jsonb`) as its JSON text, which the decoder reads as JSON.
pub const JSON: u8 = 8;
/// Set in a column name's length when the name is all ASCII.
pub const ASCII_NAME: u32 = 0x8000_0000;

/// 2^53: every integer below it in magnitude is exactly representable as an `f64`.
const MAX_EXACT_INT: f64 = 9_007_199_254_740_992.0;

/// The column-names part of a header (the count, then each name), for [`BinRows::with_names`].
///
/// # Panics
/// As [`BinRows::with_out`].
pub fn encode_names<'n, I>(columns: I) -> Vec<u8>
where
    I: IntoIterator<Item = &'n str>,
    I::IntoIter: ExactSizeIterator,
{
    let mut out = Vec::new();
    push_names(&mut out, columns);
    out
}

fn push_names<'n, I>(out: &mut (impl Out + ?Sized), columns: I)
where
    I: IntoIterator<Item = &'n str>,
    I::IntoIter: ExactSizeIterator,
{
    let columns = columns.into_iter();
    let count = u16::try_from(columns.len()).expect("more database columns than 65,535");
    out.extend(&count.to_le_bytes());
    for name in columns {
        assert!(
            name.len() < ASCII_NAME as usize,
            "database column name too long"
        );
        let ascii = if name.is_ascii() { ASCII_NAME } else { 0 };
        out.extend(&(name.len() as u32 | ascii).to_le_bytes());
        out.extend(name.as_bytes());
    }
}

/// Builds a result set into `O` (a `Vec<u8>`, or a Velt `u8[]` itself, a native library's
/// `velt_native::Array<u8>`: no copy): one value method per column, in column order, for every
/// row.
pub struct BinRows<O: Out = Vec<u8>> {
    buf: O,
    rows: usize,
}

impl BinRows {
    /// A writer into a `Vec<u8>` (see [`BinRows::with_out`]).
    pub fn new<'n, I>(columns: I, single: bool) -> BinRows
    where
        I: IntoIterator<Item = &'n str>,
        I::IntoIter: ExactSizeIterator,
    {
        BinRows::with_out(Vec::with_capacity(256), columns, single)
    }
}

impl<O: Out> BinRows<O> {
    /// A writer for rows with these column names into `out`; `single` for one row read as an
    /// object rather than an array of rows.
    ///
    /// # Panics
    /// With more than 65,535 columns or a name of 2 GiB or more (beyond what databases allow).
    pub fn with_out<'n, I>(mut out: O, columns: I, single: bool) -> BinRows<O>
    where
        I: IntoIterator<Item = &'n str>,
        I::IntoIter: ExactSizeIterator,
    {
        out.reserve(256);
        out.push(single as u8);
        push_names(&mut out, columns);
        BinRows { buf: out, rows: 0 }
    }

    /// [`BinRows::with_out`] with the column names encoded by [`encode_names`] (a driver keeps
    /// them per statement rather than asking the database for them on every query).
    pub fn with_names(mut out: O, names: &[u8], single: bool) -> BinRows<O> {
        out.reserve(names.len() + 256);
        out.push(single as u8);
        out.extend(names);
        BinRows { buf: out, rows: 0 }
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
        self.buf.extend(&v.to_le_bytes());
    }

    /// A float; NaN and ±Infinity are `null`, and -0 is 0, as in the JSON text the rows
    /// stand for.
    pub fn float(&mut self, v: f64) {
        if !v.is_finite() {
            return self.null();
        }
        let v = if v == 0.0 { 0.0 } else { v };
        self.buf.push(FLOAT);
        self.buf.extend(&v.to_le_bytes());
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
    /// A nested value as JSON text (well-formed: the reader trusts it to be one value).
    pub fn json(&mut self, v: &[u8]) {
        self.sized(JSON, v);
    }

    pub fn bytes(&mut self, v: &[u8]) {
        self.sized(BLOB, v);
    }

    fn sized(&mut self, tag: u8, v: &[u8]) {
        assert!(v.len() <= u32::MAX as usize, "database value too long");
        self.buf.reserve(v.len() + 5);
        self.buf.push(tag);
        self.buf.extend(&(v.len() as u32).to_le_bytes());
        self.buf.extend(v);
    }

    /// The finished buffer.
    pub fn finish(self) -> O {
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
    /// A nested value's JSON text.
    Json(usize, usize),
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
        tag @ (TEXT | ASCII | BLOB | JSON) => {
            let b = buf.get(at..at + 4)?;
            let start = at + 4;
            let end = start.checked_add(u32::from_le_bytes(b.try_into().ok()?) as usize)?;
            if end > buf.len() {
                return None;
            }
            if tag == BLOB {
                (BinCell::Blob(start, end), end)
            } else if tag == JSON {
                (BinCell::Json(start, end), end)
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
    push_cell(
        out,
        match cell {
            BinCell::Null => Cell::Null,
            BinCell::Bool(v) => Cell::Bool(v),
            BinCell::Int(v) => Cell::Int(v),
            BinCell::Float(v) => Cell::Float(v),
            BinCell::Text(start, end, _) => Cell::Text(&buf[start..end]),
            BinCell::Blob(start, end) => Cell::Blob(&buf[start..end]),
            BinCell::Json(start, end) => Cell::Json(&buf[start..end]),
        },
    );
}

/// [`push_cell_json`] of a cell however it is held (a live cursor's,
/// [`crate::rows_cursor`]); text that is not UTF-8 gets U+FFFD.
pub fn push_cell(out: &mut Vec<u8>, cell: Cell<'_>) {
    match cell {
        Cell::Null => out.extend_from_slice(b"null"),
        Cell::Bool(v) => out.extend_from_slice(if v { b"true" } else { b"false" }),
        Cell::Int(v) => crate::fmt::push_i64(out, v),
        Cell::Float(v) if v.fract() == 0.0 && v.abs() < MAX_EXACT_INT => {
            crate::fmt::push_i64(out, v as i64)
        }
        Cell::Float(v) => crate::fmt::push_f64(out, v),
        Cell::Ascii(t) => push_json_string(out, t),
        Cell::Json(j) => out.extend_from_slice(j),
        Cell::Text(t) => match std::str::from_utf8(t) {
            Ok(_) => push_json_string(out, t),
            Err(_) => push_json_string(out, String::from_utf8_lossy(t).as_bytes()),
        },
        Cell::Blob(b) => {
            out.push(b'[');
            for (i, &v) in b.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                crate::fmt::push_u64(out, v as u64);
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
