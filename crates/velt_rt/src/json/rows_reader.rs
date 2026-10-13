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
    ReadError, FLAG_REJECT_UNKNOWN, STEP_END, STEP_ERROR, STEP_MORE, TOKEN_ARRAY_END,
    TOKEN_ARRAY_START, TOKEN_EOF, TOKEN_ERROR, TOKEN_FALSE, TOKEN_NULL, TOKEN_NUMBER,
    TOKEN_OBJECT_END, TOKEN_OBJECT_START, TOKEN_STRING, TOKEN_TRUE,
};
use super::rows_bin::{self as bin, BinCell};
use super::value::Value;
use crate::str::VeltStr;
use std::sync::Arc;

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
    /// Offset of the next unread row, cell or blob byte.
    pos: usize,
    col: usize,
    /// End of the blob being read (in `Bytes` / `Byte`).
    blob_end: usize,
    /// Offset in the header of column `col`'s name (its length prefix).
    name: usize,
}

pub struct RowReader {
    buf: Vec<u8>,
    columns: usize,
    single: bool,
    c: Cursor,
    error: Option<ReadError>,
    reject_unknown: bool,
    /// Positions `mark` saved (none until a decoder looks ahead).
    marks: Vec<Cursor>,
}

/// 2^63 as a float: integral floats in `[-2^63, 2^63)` are exact `i64`s.
const I64_LIMIT: f64 = 9_223_372_036_854_775_808.0;

impl RowReader {
    /// A reader over `buf` (from `rows_bin::BinRows`) with `velt_rt_json_reader_new_with` flags.
    pub fn new(buf: Vec<u8>, flags: u32) -> RowReader {
        // A buffer without a readable header has no rows.
        let (single, columns, start) = bin::header_end(&buf).unwrap_or((false, 0, buf.len()));
        RowReader {
            buf,
            columns,
            single,
            c: Cursor {
                at: At::Top,
                pos: start,
                col: 0,
                blob_end: 0,
                name: bin::NAMES,
            },
            error: None,
            reject_unknown: flags & FLAG_REJECT_UNKNOWN != 0,
            marks: Vec::new(),
        }
    }

    fn mismatch(&mut self) -> u8 {
        self.error.get_or_insert(ReadError::Mismatch);
        0
    }

    /// The pending column value and where the next cell starts.
    #[inline]
    fn val(&mut self) -> Option<(BinCell, usize)> {
        if self.error.is_some() {
            return None;
        }
        if self.c.at != At::Val {
            self.mismatch();
            return None;
        }
        let cell = bin::cell_at(&self.buf, self.c.pos);
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
        let b = *self.buf.get(self.c.pos)?;
        self.c.pos += 1;
        self.c.at = At::Bytes;
        Some(b)
    }

    pub fn peek(&mut self) -> u32 {
        if self.error.is_some() {
            return TOKEN_ERROR;
        }
        match self.c.at {
            At::Top if self.single => TOKEN_OBJECT_START,
            At::Top => TOKEN_ARRAY_START,
            At::Rows if self.c.pos < self.buf.len() => TOKEN_OBJECT_START,
            At::Rows => TOKEN_ARRAY_END,
            At::Row => TOKEN_OBJECT_START,
            At::Keys if self.c.col < self.columns => TOKEN_STRING,
            At::Keys => TOKEN_OBJECT_END,
            At::Val => match bin::cell_at(&self.buf, self.c.pos) {
                Some((BinCell::Null, _)) => TOKEN_NULL,
                Some((BinCell::Bool(true), _)) => TOKEN_TRUE,
                Some((BinCell::Bool(false), _)) => TOKEN_FALSE,
                Some((BinCell::Int(_) | BinCell::Float(_), _)) => TOKEN_NUMBER,
                Some((BinCell::Text(..), _)) => TOKEN_STRING,
                Some((BinCell::Blob(..), _)) => TOKEN_ARRAY_START,
                None => TOKEN_ERROR,
            },
            At::Bytes if self.c.pos < self.c.blob_end => TOKEN_NUMBER,
            At::Bytes => TOKEN_ARRAY_END,
            At::Byte => TOKEN_NUMBER,
            At::Done => TOKEN_EOF,
        }
    }

    /// Consume `{` or `[`.
    pub fn open(&mut self, bracket: u8) -> u8 {
        if self.error.is_some() {
            return 0;
        }
        match (self.c.at, bracket) {
            (At::Top, b'[') if !self.single => self.c.at = At::Rows,
            (At::Top, b'{') if self.single => self.open_row(),
            (At::Row, b'{') => self.open_row(),
            (At::Val, b'[') => match bin::cell_at(&self.buf, self.c.pos) {
                Some((BinCell::Blob(start, end), _)) => {
                    self.c.at = At::Bytes;
                    self.c.pos = start;
                    self.c.blob_end = end;
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
        if self.error.is_some() {
            return STEP_ERROR;
        }
        match self.c.at {
            At::Rows if self.c.pos < self.buf.len() => {
                self.c.at = At::Row;
                STEP_MORE
            }
            At::Rows => {
                self.c.at = At::Done;
                STEP_END
            }
            At::Bytes if self.c.pos < self.c.blob_end => {
                self.c.at = At::Byte;
                STEP_MORE
            }
            At::Bytes => {
                let end = self.c.blob_end;
                self.advance(end);
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
        let (start, end, ascii) = bin::name_at(&self.buf, self.c.name);
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
        match self.val()? {
            (BinCell::Text(start, end, ascii), next) => {
                // SAFETY: the writer stored valid UTF-8 (`BinRows::text`).
                let text = unsafe { std::str::from_utf8_unchecked(&self.buf[start..end]) };
                let s = if text.is_empty() {
                    VeltStr::empty()
                } else if ascii {
                    // SAFETY: the writer found only ASCII, one UTF-16 unit per byte.
                    unsafe { VeltStr::from_text_counted(text, text.len()) }
                } else {
                    VeltStr::from_text(text)
                };
                self.advance(next);
                Some(s)
            }
            _ => {
                self.mismatch();
                None
            }
        }
    }

    pub fn f64(&mut self) -> Option<f64> {
        if self.c.at == At::Byte && self.error.is_none() {
            return self.byte().map(f64::from);
        }
        let (v, next) = match self.val()? {
            (BinCell::Int(v), next) => (v as f64, next),
            (BinCell::Float(v), next) => (v, next),
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
        if self.c.at == At::Byte && self.error.is_none() {
            return self.byte().map(i64::from);
        }
        let (v, next) = match self.val()? {
            (BinCell::Int(v), next) => (v, next),
            (BinCell::Float(v), next)
                if v.fract() == 0.0 && (-I64_LIMIT..I64_LIMIT).contains(&v) =>
            {
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
        match self.val()? {
            (BinCell::Bool(v), next) => {
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
        match self.val() {
            Some((BinCell::Null, next)) => {
                self.advance(next);
                1
            }
            Some(_) => self.mismatch(),
            None => 0,
        }
    }

    /// Where the row starting at `pos` ends.
    fn row_end(&self, mut pos: usize) -> Option<usize> {
        for _ in 0..self.columns {
            pos = bin::cell_at(&self.buf, pos)?.1;
        }
        Some(pos)
    }

    /// Skip one complete value of any kind.
    pub fn skip(&mut self) -> u8 {
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
            At::Val => match bin::cell_at(&self.buf, self.c.pos) {
                Some((_, next)) => self.advance(next),
                None => return self.mismatch(),
            },
            At::Byte => {
                self.c.pos += 1;
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
        self.marks.push(self.c);
        (self.marks.len() - 1) as u64
    }

    /// Go back to `mark` and forget any error since.
    pub fn reset(&mut self, mark: u64) {
        if let Some(&c) = self.marks.get(mark as usize) {
            self.c = c;
            self.error = None;
        }
    }

    /// One complete value of any kind as a `json.Value` tree: built from the JSON text the
    /// value stands for (a decoder asking for a dynamic value is the rare case).
    pub fn value(&mut self) -> Option<Arc<Value>> {
        if self.error.is_some() {
            return None;
        }
        let mut text = Vec::new();
        let ok = match self.c.at {
            At::Top => match bin::to_json(&self.buf) {
                Some(json) => {
                    text = json;
                    true
                }
                None => false,
            },
            At::Row => bin::push_row_json(&mut text, &self.buf, self.c.pos).is_some(),
            At::Val => match bin::cell_at(&self.buf, self.c.pos) {
                Some((cell, _)) => {
                    bin::push_cell_json(&mut text, &self.buf, cell);
                    true
                }
                None => false,
            },
            At::Byte => match self.buf.get(self.c.pos) {
                Some(&b) => {
                    crate::fmt::push_u64(&mut text, b as u64);
                    true
                }
                None => false,
            },
            _ => false,
        };
        let value = match ok {
            true => super::value::parse(&text, usize::MAX).ok(),
            false => None,
        };
        if value.is_none() || self.skip() == 0 {
            self.mismatch();
            return None;
        }
        value
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::rows_bin::BinRows;

    /// Two rows of every kind of cell.
    fn sample(single: bool) -> Vec<u8> {
        let mut w = BinRows::new(["id", "name", "score", "ok", "data", "nöte"], single);
        w.int(7);
        w.text(b"ada \"x\"");
        w.float(1.5);
        w.bool(true);
        w.bytes(&[1, 255]);
        w.null();
        w.end_row();
        if !single {
            w.int(-1);
            w.text(b"");
            w.float(3.0);
            w.bool(false);
            w.bytes(&[]);
            w.text("é".as_bytes());
            w.end_row();
        }
        w.finish()
    }

    fn key(r: &mut RowReader) -> Option<String> {
        r.next_key().unwrap().map(|k| {
            // SAFETY: a key the reader just made; released once.
            unsafe {
                let text = k.to_string_lossy();
                let mut k = k;
                crate::str::velt_rt_str_drop(&mut k);
                text
            }
        })
    }

    fn text(r: &mut RowReader) -> String {
        let s = r.string().unwrap();
        // SAFETY: as in `key`.
        unsafe {
            let text = s.to_string_lossy();
            let mut s = s;
            crate::str::velt_rt_str_drop(&mut s);
            text
        }
    }

    #[test]
    fn reads_rows_as_an_array_of_objects() {
        let mut r = RowReader::new(sample(false), 0);
        assert_eq!(r.peek(), TOKEN_ARRAY_START);
        assert_eq!(r.open(b'['), 1);
        assert_eq!(r.array_next(), STEP_MORE);
        assert_eq!(r.peek(), TOKEN_OBJECT_START);
        assert_eq!(r.open(b'{'), 1);
        assert_eq!(key(&mut r).as_deref(), Some("id"));
        assert_eq!(r.peek(), TOKEN_NUMBER);
        assert_eq!(r.i64(), Some(7));
        assert_eq!(key(&mut r).as_deref(), Some("name"));
        assert_eq!(text(&mut r), "ada \"x\"");
        assert_eq!(key(&mut r).as_deref(), Some("score"));
        assert_eq!(r.f64(), Some(1.5));
        assert_eq!(key(&mut r).as_deref(), Some("ok"));
        assert_eq!(r.peek(), TOKEN_TRUE);
        assert_eq!(r.bool(), Some(true));
        assert_eq!(key(&mut r).as_deref(), Some("data"));
        assert_eq!(r.peek(), TOKEN_ARRAY_START);
        assert_eq!(r.open(b'['), 1);
        assert_eq!(r.array_next(), STEP_MORE);
        assert_eq!(r.i64(), Some(1));
        assert_eq!(r.array_next(), STEP_MORE);
        assert_eq!(r.f64(), Some(255.0));
        assert_eq!(r.array_next(), STEP_END);
        assert_eq!(key(&mut r).as_deref(), Some("nöte"));
        assert_eq!(r.peek(), TOKEN_NULL);
        assert_eq!(r.null(), 1);
        assert_eq!(key(&mut r), None);
        assert_eq!(r.array_next(), STEP_MORE);
        assert_eq!(r.open(b'{'), 1);
        assert_eq!(key(&mut r).as_deref(), Some("id"));
        assert_eq!(r.f64(), Some(-1.0));
        assert_eq!(key(&mut r).as_deref(), Some("name"));
        assert_eq!(text(&mut r), "");
        assert_eq!(key(&mut r).as_deref(), Some("score"));
        // An integral float is an integer too, as `3.0` is in JSON text.
        assert_eq!(r.i64(), Some(3));
        assert_eq!(key(&mut r).as_deref(), Some("ok"));
        assert_eq!(r.bool(), Some(false));
        assert_eq!(key(&mut r).as_deref(), Some("data"));
        assert_eq!(r.open(b'['), 1);
        assert_eq!(r.array_next(), STEP_END);
        assert_eq!(key(&mut r).as_deref(), Some("nöte"));
        assert_eq!(text(&mut r), "é");
        assert_eq!(key(&mut r), None);
        assert_eq!(r.array_next(), STEP_END);
        assert_eq!(r.peek(), TOKEN_EOF);
        assert_eq!(r.end(), 1);
    }

    #[test]
    fn a_single_row_is_an_object() {
        let mut r = RowReader::new(sample(true), 0);
        assert_eq!(r.peek(), TOKEN_OBJECT_START);
        assert_eq!(r.open(b'['), 0);
        assert_eq!(r.message("array", "$"), "expected array at $");
        let mut r = RowReader::new(sample(true), 0);
        assert_eq!(r.open(b'{'), 1);
        for _ in 0..6 {
            assert!(key(&mut r).is_some());
            assert_eq!(r.skip(), 1);
        }
        assert_eq!(key(&mut r), None);
        assert_eq!(r.end(), 1);
    }

    #[test]
    fn no_rows_is_an_empty_array() {
        let mut r = RowReader::new(BinRows::new(["a"], false).finish(), 0);
        assert_eq!(r.open(b'['), 1);
        assert_eq!(r.peek(), TOKEN_ARRAY_END);
        assert_eq!(r.array_next(), STEP_END);
        assert_eq!(r.end(), 1);
    }

    #[test]
    fn a_wrong_kind_fails_until_reset() {
        let mut r = RowReader::new(sample(false), 0);
        r.open(b'[');
        r.array_next();
        r.open(b'{');
        key(&mut r);
        let mark = r.mark();
        assert!(r.string().is_none());
        assert_eq!(r.peek(), TOKEN_ERROR);
        assert_eq!(r.i64(), None);
        assert_eq!(r.message("string", "$[0].id"), "expected string at $[0].id");
        r.reset(mark);
        assert_eq!(r.i64(), Some(7));
        key(&mut r);
        // A non-integral float is no integer; a bool no number; text no bool.
        assert_eq!(r.bool(), None);
        r.reset(mark);
        assert_eq!(r.skip(), 1);
        assert_eq!(key(&mut r).as_deref(), Some("name"));
        assert_eq!(r.skip(), 1);
        key(&mut r);
        assert_eq!(r.i64(), None);
    }

    #[test]
    fn unknown_keys_are_skipped_or_rejected() {
        let mut r = RowReader::new(sample(true), 0);
        r.open(b'{');
        key(&mut r);
        assert_eq!(r.skip_unknown(), 1);
        assert_eq!(key(&mut r).as_deref(), Some("name"));
        let mut r = RowReader::new(sample(true), FLAG_REJECT_UNKNOWN);
        r.open(b'{');
        key(&mut r);
        assert_eq!(r.skip_unknown(), 0);
        assert_eq!(r.message("", "$.id"), "unknown field at $.id");
    }

    #[test]
    fn whole_rows_skip_and_read_as_values() {
        let mut r = RowReader::new(sample(false), 0);
        r.open(b'[');
        assert_eq!(r.array_next(), STEP_MORE);
        assert_eq!(r.skip(), 1);
        assert_eq!(r.array_next(), STEP_MORE);
        let row = r.value().unwrap();
        assert!(matches!(&*row, Value::Object(o) if o.len() == 6));
        assert_eq!(r.array_next(), STEP_END);
        let mut r = RowReader::new(sample(false), 0);
        assert!(matches!(&*r.value().unwrap(), Value::Array(rows) if rows.len() == 2));
        assert_eq!(r.end(), 1);
    }

    #[test]
    fn a_damaged_buffer_is_a_mismatch_not_a_crash() {
        let whole = sample(false);
        for cut in 0..whole.len() {
            let mut r = RowReader::new(whole[..cut].to_vec(), 0);
            if r.open(b'[') == 0 {
                continue;
            }
            while r.array_next() == STEP_MORE {
                if r.open(b'{') == 0 {
                    break;
                }
                while let Ok(Some(k)) = r.next_key() {
                    let mut k = k;
                    // SAFETY: a key the reader just made; released once.
                    unsafe { crate::str::velt_rt_str_drop(&mut k) };
                    if r.skip() == 0 {
                        break;
                    }
                }
            }
        }
    }
}
