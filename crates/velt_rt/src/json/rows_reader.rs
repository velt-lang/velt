//! The pull reader over binary result rows ([`super::rows_bin`], #33): what
//! `velt_rt_json_reader_*` do when the reader was made from a driver's parked rows instead of
//! JSON text. It answers exactly as the text reader would for the JSON those rows stand for
//! (`[{"col":value,...},...]`, or one row's object), so the generated decoders need not know.
//!
//! Like the text reader, the first failure is sticky until `reset`. The only failures are a
//! value of another kind than the decoder asked for and a rejected unknown key: the rows are
//! well-formed by construction: only `rows_bin::BinRows` makes them, in this process. A buffer
//! cut short reads as a mismatch, never out of bounds; what the writer says about its text
//! (UTF-8, all ASCII) is trusted.

use super::error::{mismatch_message, unknown_message};
use super::reader::{
    ReadError, Reader, FLAG_REJECT_UNKNOWN, STEP_END, STEP_ERROR, STEP_MORE, TOKEN_ARRAY_END,
    TOKEN_ARRAY_START, TOKEN_EOF, TOKEN_ERROR, TOKEN_FALSE, TOKEN_NULL, TOKEN_NUMBER,
    TOKEN_OBJECT_END, TOKEN_OBJECT_START, TOKEN_STRING, TOKEN_TRUE,
};
use super::rows_bin::{self as bin, BinCell};
use crate::str::VeltStr;
use velt_text::rows::Cell;

/// Where the reader is in the document.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum At {
    /// Before the top-level value.
    Top,
    /// In the array of rows, before `,` or `]`.
    Rows,
    /// Before a row's `{`.
    Row,
    /// In a row, before the key of column `col` (or `}` after the last).
    Keys,
    /// Before the value of column `col`.
    Val,
    /// In a blob's array of bytes, before `,` or `]`.
    Bytes,
    /// Before a blob's byte.
    Byte,
    /// After the top-level value.
    Done,
}

/// A position: what `mark` saves and `reset` restores.
#[derive(Clone, Copy)]
struct Cursor {
    at: At,
    /// Offset of the next unread row or cell.
    pos: usize,
    col: usize,
    /// The blob being read (in `Bytes` / `Byte`): its address and length, the next byte's
    /// index, and where the cell after it starts.
    blob: usize,
    blob_len: usize,
    blob_at: usize,
    after: usize,
    /// Offset in the header of column `col`'s name (its length prefix).
    name: usize,
}

pub struct RowReader {
    /// The rows: a `u8[]` the decoder borrows for the whole parse (as the text reader borrows
    /// its source string), or `_owned`'s bytes.
    buf: &'static [u8],
    _owned: Vec<u8>,
    columns: usize,
    single: bool,
    c: Cursor,
    error: Option<ReadError>,
    reject_unknown: bool,
    /// Positions `mark` saved (none until a decoder looks ahead), with the nested reader's own
    /// mark when one was reading.
    marks: Vec<(Cursor, Option<u64>)>,
    /// Reads the column value that is a nested value's JSON text (`Cell::Json`), while it does:
    /// every call goes to it until the value is read.
    nested: Option<Box<Reader>>,
    flags: u32,
    /// How many marks there were when the array of rows was opened: a decoder looks ahead
    /// within one value, so the next row forgets the marks made in the last one.
    row_marks: usize,
}

/// 2^63 as a float: integral floats in `[-2^63, 2^63)` are exact `i64`s.
const I64_LIMIT: f64 = 9_223_372_036_854_775_808.0;

impl RowReader {
    /// A reader over `buf` (from `rows_bin::BinRows`) with `velt_rt_json_reader_new_with` flags.
    pub fn new(buf: Vec<u8>, flags: u32) -> RowReader {
        // SAFETY: the bytes live in `_owned`'s heap buffer, which moves with the reader but
        // never changes.
        let bytes = unsafe { std::mem::transmute::<&[u8], &'static [u8]>(buf.as_slice()) };
        RowReader::with(bytes, buf, flags)
    }

    /// A reader over rows the caller keeps alive and unchanged until the reader is dropped.
    ///
    /// # Safety
    /// As said: `buf` outlives the reader.
    pub unsafe fn borrowed(buf: &[u8], flags: u32) -> RowReader {
        RowReader::with(
            std::mem::transmute::<&[u8], &'static [u8]>(buf),
            Vec::new(),
            flags,
        )
    }

    fn with(buf: &'static [u8], owned: Vec<u8>, flags: u32) -> RowReader {
        // A buffer without a readable header has no rows.
        let (single, columns, start) = bin::header_end(buf).unwrap_or((false, 0, buf.len()));
        RowReader {
            buf,
            _owned: owned,
            columns,
            single,
            c: Cursor {
                at: At::Top,
                pos: start,
                col: 0,
                blob: 0,
                blob_len: 0,
                blob_at: 0,
                after: 0,
                name: bin::NAMES,
            },
            error: None,
            reject_unknown: flags & FLAG_REJECT_UNKNOWN != 0,
            marks: Vec::new(),
            row_marks: 0,
            nested: None,
            flags,
        }
    }

    fn mismatch(&mut self) -> u8 {
        self.error.get_or_insert(ReadError::Mismatch);
        0
    }

    /// The reader of the pending value when it is a nested value's JSON text (made on first
    /// use), else None.
    fn nest(&mut self) -> Option<&mut Reader> {
        if self.nested.is_none() {
            if self.error.is_some() || self.c.at != At::Val {
                return None;
            }
            let Some((Cell::Json(text), _)) = self.cell() else {
                return None;
            };
            self.nested = Some(Box::new(self.json_reader(text)));
        }
        self.nested.as_deref_mut()
    }

    /// A reader of a nested value's text (a driver's well-formed, UTF-8 JSON).
    fn json_reader(&self, text: &'static [u8]) -> Reader {
        Reader::with_source(text, true, self.flags & FLAG_REJECT_UNKNOWN, 0)
    }

    /// After a call the nested reader took: its failure is this reader's; once its value is read,
    /// the column is.
    fn nested_done(&mut self) {
        let Some(r) = &self.nested else { return };
        match r.error() {
            Some(ReadError::Unknown) => {
                self.error.get_or_insert(ReadError::Unknown);
            }
            Some(_) => {
                self.error.get_or_insert(ReadError::Mismatch);
            }
            None if r.depth() == 0 => {
                self.nested = None;
                if let Some((_, next)) = self.cell() {
                    self.advance(next);
                }
            }
            None => {}
        }
    }

    /// The cell of column `col` and where the next one starts.
    #[inline]
    fn cell(&self) -> Option<(Cell<'static>, usize)> {
        bin::cell_at(self.buf, self.c.pos).map(|(cell, next)| {
            let cell = match cell {
                BinCell::Null => Cell::Null,
                BinCell::Bool(v) => Cell::Bool(v),
                BinCell::Int(v) => Cell::Int(v),
                BinCell::Float(v) => Cell::Float(v),
                BinCell::Text(start, end, true) => Cell::Ascii(&self.buf[start..end]),
                BinCell::Text(start, end, false) => Cell::Text(&self.buf[start..end]),
                BinCell::Blob(start, end) => Cell::Blob(&self.buf[start..end]),
                BinCell::Json(start, end) => Cell::Json(&self.buf[start..end]),
            };
            (cell, next)
        })
    }

    /// Whether another row follows.
    fn row_ahead(&self) -> bool {
        self.c.pos < self.buf.len()
    }

    /// The pending column value and where the next cell starts.
    #[inline]
    fn val(&mut self) -> Option<(Cell<'static>, usize)> {
        if self.error.is_some() {
            return None;
        }
        if self.c.at != At::Val {
            self.mismatch();
            return None;
        }
        let cell = self.cell();
        if cell.is_none() {
            self.mismatch();
        }
        cell
    }

    /// The pending value was consumed up to `next`.
    #[inline]
    fn advance(&mut self, next: usize) {
        self.c.pos = next;
        self.c.col += 1;
        self.c.at = At::Keys;
    }

    /// The pending blob byte, consumed.
    fn byte(&mut self) -> Option<u8> {
        if self.c.blob_at >= self.c.blob_len {
            return None;
        }
        // SAFETY: in the blob, which lives as long as the buffer.
        let b = unsafe { *(self.c.blob as *const u8).add(self.c.blob_at) };
        self.c.blob_at += 1;
        self.c.at = At::Bytes;
        Some(b)
    }

    pub fn peek(&mut self) -> u32 {
        if let Some(r) = self.nest() {
            return r.peek();
        }
        if self.error.is_some() {
            return TOKEN_ERROR;
        }
        let at = self.c.at;
        match at {
            At::Top if self.single => TOKEN_OBJECT_START,
            At::Top => TOKEN_ARRAY_START,
            At::Rows if self.row_ahead() => TOKEN_OBJECT_START,
            At::Rows => TOKEN_ARRAY_END,
            At::Row => TOKEN_OBJECT_START,
            At::Keys if self.c.col < self.columns => TOKEN_STRING,
            At::Keys => TOKEN_OBJECT_END,
            At::Val => match self.cell() {
                Some((Cell::Null, _)) => TOKEN_NULL,
                Some((Cell::Bool(true), _)) => TOKEN_TRUE,
                Some((Cell::Bool(false), _)) => TOKEN_FALSE,
                Some((Cell::Int(_) | Cell::Float(_), _)) => TOKEN_NUMBER,
                Some((Cell::Text(_) | Cell::Ascii(_), _)) => TOKEN_STRING,
                Some((Cell::Blob(_), _)) => TOKEN_ARRAY_START,
                // Read by the nested reader (`nest`), before this.
                Some((Cell::Json(_), _)) | None => TOKEN_ERROR,
            },
            At::Bytes if self.c.blob_at < self.c.blob_len => TOKEN_NUMBER,
            At::Bytes => TOKEN_ARRAY_END,
            At::Byte => TOKEN_NUMBER,
            At::Done => TOKEN_EOF,
        }
    }

    /// Consume `{` or `[`.
    pub fn open(&mut self, bracket: u8) -> u8 {
        if let Some(r) = self.nest() {
            let v = r.open(bracket);
            self.nested_done();
            return v;
        }
        if self.error.is_some() {
            return 0;
        }
        match (self.c.at, bracket) {
            (At::Top, b'[') if !self.single => {
                self.c.at = At::Rows;
                self.row_marks = self.marks.len();
            }
            (At::Top, b'{') if self.single => self.open_row(),
            (At::Row, b'{') => self.open_row(),
            (At::Val, b'[') => match self.cell() {
                Some((Cell::Blob(b), next)) => {
                    self.c.at = At::Bytes;
                    self.c.blob = b.as_ptr() as usize;
                    self.c.blob_len = b.len();
                    self.c.blob_at = 0;
                    self.c.after = next;
                }
                _ => return self.mismatch(),
            },
            _ => return self.mismatch(),
        }
        1
    }

    fn open_row(&mut self) {
        self.c.at = At::Keys;
        self.c.col = 0;
        self.c.name = bin::NAMES;
    }

    pub fn array_next(&mut self) -> u8 {
        if let Some(r) = self.nest() {
            let v = r.array_next();
            self.nested_done();
            return v;
        }
        if self.error.is_some() {
            return STEP_ERROR;
        }
        let at = self.c.at;
        match at {
            At::Rows if self.row_ahead() => {
                self.c.at = At::Row;
                self.marks.truncate(self.row_marks);
                STEP_MORE
            }
            At::Rows => {
                self.c.at = At::Done;
                STEP_END
            }
            At::Bytes if self.c.blob_at < self.c.blob_len => {
                self.c.at = At::Byte;
                STEP_MORE
            }
            At::Bytes => {
                let after = self.c.after;
                self.advance(after);
                STEP_END
            }
            _ => {
                self.mismatch();
                STEP_ERROR
            }
        }
    }

    /// The next column's name (borrowed from the buffer when ASCII, which the reader owns until
    /// it is freed, as the text reader's keys borrow its source); `Ok(None)` after the last.
    pub(crate) fn next_key(&mut self) -> Result<Option<VeltStr>, ()> {
        if let Some(r) = self.nest() {
            let k = r.next_key().map(|k| k.map(|k| r.borrowed_str(k)));
            self.nested_done();
            return k;
        }
        if self.error.is_some() {
            return Err(());
        }
        if self.c.at != At::Keys {
            self.mismatch();
            return Err(());
        }
        if self.c.col >= self.columns {
            self.c.at = if self.single { At::Done } else { At::Rows };
            return Ok(None);
        }
        // In bounds: `header_end` walked every name.
        let (start, end, ascii) = bin::name_at(self.buf, self.c.name);
        self.c.name = end;
        self.c.at = At::Val;
        let name = &self.buf[start..end];
        Ok(Some(if ascii {
            // SAFETY: ASCII is canonical WTF-8 with one unit per byte; the buffer is not
            // changed or freed before the reader, which outlives the keys it hands out.
            unsafe { VeltStr::borrowed_text(name.as_ptr(), name.len(), name.len()) }
        } else {
            VeltStr::from_bytes(name)
        }))
    }

    pub fn string(&mut self) -> Option<VeltStr> {
        if let Some(r) = self.nest() {
            let v = r.string().map(|t| r.owned_str(t));
            self.nested_done();
            return v;
        }
        let (s, next) = match self.val()? {
            (Cell::Ascii(t), next) => (ascii_str(t), next),
            (Cell::Text(t), next) if t.is_ascii() => (ascii_str(t), next),
            (Cell::Text(t), next) => match std::str::from_utf8(t) {
                Ok(text) => (VeltStr::from_text(text), next),
                Err(_) => (VeltStr::from_text(&String::from_utf8_lossy(t)), next),
            },
            _ => {
                self.mismatch();
                return None;
            }
        };
        self.advance(next);
        Some(s)
    }

    pub fn f64(&mut self) -> Option<f64> {
        if let Some(r) = self.nest() {
            let v = r.f64();
            self.nested_done();
            return v;
        }
        if self.c.at == At::Byte && self.error.is_none() {
            return self.byte().map(f64::from);
        }
        let (v, next) = match self.val()? {
            (Cell::Int(v), next) => (v as f64, next),
            (Cell::Float(v), next) => (v, next),
            _ => {
                self.mismatch();
                return None;
            }
        };
        self.advance(next);
        Some(v)
    }

    /// An integer, or a float that is one exactly (the text reader accepts `3.0` too).
    pub fn i64(&mut self) -> Option<i64> {
        if let Some(r) = self.nest() {
            let v = r.i64();
            self.nested_done();
            return v;
        }
        if self.c.at == At::Byte && self.error.is_none() {
            return self.byte().map(i64::from);
        }
        let (v, next) = match self.val()? {
            (Cell::Int(v), next) => (v, next),
            (Cell::Float(v), next) if v.fract() == 0.0 && (-I64_LIMIT..I64_LIMIT).contains(&v) => {
                (v as i64, next)
            }
            _ => {
                self.mismatch();
                return None;
            }
        };
        self.advance(next);
        Some(v)
    }

    pub fn bool(&mut self) -> Option<bool> {
        if let Some(r) = self.nest() {
            let v = r.bool();
            self.nested_done();
            return v;
        }
        match self.val()? {
            (Cell::Bool(v), next) => {
                self.advance(next);
                Some(v)
            }
            _ => {
                self.mismatch();
                None
            }
        }
    }

    pub fn null(&mut self) -> u8 {
        if let Some(r) = self.nest() {
            let v = r.null();
            self.nested_done();
            return v;
        }
        match self.val() {
            Some((Cell::Null, next)) => {
                self.advance(next);
                1
            }
            Some(_) => self.mismatch(),
            None => 0,
        }
    }

    /// Where the row starting at `pos` ends (binary rows).
    fn row_end(&self, mut pos: usize) -> Option<usize> {
        for _ in 0..self.columns {
            pos = bin::cell_at(self.buf, pos)?.1;
        }
        Some(pos)
    }

    /// Skip one complete value of any kind.
    pub fn skip(&mut self) -> u8 {
        if let Some(r) = self.nest() {
            let v = r.skip();
            self.nested_done();
            return v;
        }
        if self.error.is_some() {
            return 0;
        }
        match self.c.at {
            At::Top => self.c.at = At::Done,
            At::Row => match self.row_end(self.c.pos) {
                Some(end) => {
                    self.c.pos = end;
                    self.c.at = At::Rows;
                }
                None => return self.mismatch(),
            },
            At::Val => match self.cell() {
                Some((_, next)) => self.advance(next),
                None => return self.mismatch(),
            },
            At::Byte => {
                self.c.blob_at += 1;
                self.c.at = At::Bytes;
            }
            _ => return self.mismatch(),
        }
        1
    }

    pub fn skip_unknown(&mut self) -> u8 {
        if self.reject_unknown {
            self.error.get_or_insert(ReadError::Unknown);
            return 0;
        }
        self.skip()
    }

    /// The current position, for `reset`.
    pub fn mark(&mut self) -> u64 {
        let inner = self.nested.as_deref().map(Reader::mark);
        self.marks.push((self.c, inner));
        (self.marks.len() - 1) as u64
    }

    /// Go back to `mark` and forget any error since.
    pub fn reset(&mut self, mark: u64) {
        if let Some(&(c, inner)) = self.marks.get(mark as usize) {
            self.c = c;
            self.error = None;
            // Inside a nested value: its reader again, at its own mark (a fresh one over the
            // same text when it had finished since); before one: none.
            self.nested = None;
            if let Some(m) = inner {
                if let Some(r) = self.nest() {
                    r.reset(m);
                }
            }
        }
    }

    /// One complete value of any kind as the JSON text it stands for (std reads it as a
    /// `JsonValue`; a decoder asking for a dynamic value is the rare case).
    pub fn value(&mut self) -> Option<Vec<u8>> {
        if let Some(r) = self.nested.as_deref_mut() {
            let v = r.value();
            self.nested_done();
            return v;
        }
        if self.error.is_some() {
            return None;
        }
        let mut text = Vec::new();
        let ok = match self.c.at {
            At::Top => match bin::to_json(self.buf) {
                Some(json) => {
                    text = json;
                    true
                }
                None => false,
            },
            At::Row => bin::push_row_json(&mut text, self.buf, self.c.pos).is_some(),
            At::Val => match self.cell() {
                Some((cell, _)) => {
                    bin::push_cell(&mut text, cell);
                    true
                }
                None => false,
            },
            At::Byte if self.c.blob_at < self.c.blob_len => {
                // SAFETY: in the blob (see `byte`).
                let b = unsafe { *(self.c.blob as *const u8).add(self.c.blob_at) };
                crate::fmt::push_u64(&mut text, b as u64);
                true
            }
            _ => false,
        };
        if !ok || self.skip() == 0 {
            self.mismatch();
            return None;
        }
        Some(text)
    }

    /// Nothing follows the top-level value.
    pub fn end(&mut self) -> u8 {
        if self.error.is_some() {
            return 0;
        }
        if self.c.at != At::Done {
            return self.mismatch();
        }
        1
    }

    pub fn message(&self, expected: &str, path: &str) -> String {
        match self.error {
            Some(ReadError::Unknown) => unknown_message(path),
            _ => mismatch_message(expected, path),
        }
    }
}

/// An all-ASCII string: one UTF-16 unit per byte.
fn ascii_str(t: &[u8]) -> VeltStr {
    if t.is_empty() {
        return VeltStr::empty();
    }
    // SAFETY: ASCII is UTF-8 with one unit per byte.
    unsafe { VeltStr::from_text_counted(std::str::from_utf8_unchecked(t), t.len()) }
}

#[cfg(test)]
#[path = "rows_reader_tests.rs"]
mod tests;
