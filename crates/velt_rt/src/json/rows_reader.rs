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
use super::rows_bin::{self as bin, BinCell, BinRows};
use super::value::Value;
use crate::str::VeltStr;
use std::sync::Arc;
use velt_text::rows_cursor::{Cell, RawCursor};

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
    /// Offset of the next unread row or cell (binary rows; a live cursor has its cells by
    /// column).
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
    /// its source string), or `_owned`'s bytes. Over a live cursor, its header alone.
    buf: &'static [u8],
    _owned: Vec<u8>,
    /// A driver's live rows ([`RawCursor`]) instead of binary ones, and whether it is on a row
    /// not read yet (`None`: not asked since the last row).
    live: Option<RawCursor>,
    on_row: Option<bool>,
    columns: usize,
    single: bool,
    c: Cursor,
    error: Option<ReadError>,
    reject_unknown: bool,
    /// Positions `mark` saved (none until a decoder looks ahead).
    marks: Vec<Cursor>,
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
        RowReader::with(bytes, buf, None, flags)
    }

    /// A reader over rows the caller keeps alive and unchanged until the reader is dropped.
    ///
    /// # Safety
    /// As said: `buf` outlives the reader.
    pub unsafe fn borrowed(buf: &[u8], flags: u32) -> RowReader {
        RowReader::with(
            std::mem::transmute::<&[u8], &'static [u8]>(buf),
            Vec::new(),
            None,
            flags,
        )
    }

    /// A reader over a driver's live rows (`velt_text::rows_cursor`).
    ///
    /// # Safety
    /// The cursor stays alive, and is used by nothing else, until the reader is dropped.
    pub unsafe fn live(cursor: RawCursor, flags: u32) -> RowReader {
        let header = cursor.header();
        RowReader::with(header, Vec::new(), Some(cursor), flags)
    }

    fn with(buf: &'static [u8], owned: Vec<u8>, live: Option<RawCursor>, flags: u32) -> RowReader {
        // A buffer without a readable header has no rows.
        let (single, columns, start) = bin::header_end(buf).unwrap_or((false, 0, buf.len()));
        RowReader {
            buf,
            _owned: owned,
            live,
            on_row: None,
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
        }
    }

    fn mismatch(&mut self) -> u8 {
        self.error.get_or_insert(ReadError::Mismatch);
        0
    }

    /// The cell of column `col` and where the next one starts.
    #[inline]
    fn cell(&self) -> Option<(Cell<'static>, usize)> {
        match &self.live {
            // SAFETY: the reader's contract; `col` is below the column count (`Val`).
            Some(cursor) => Some((unsafe { cursor.cell(self.c.col) }, self.c.pos)),
            None => bin::cell_at(self.buf, self.c.pos).map(|(cell, next)| {
                let cell = match cell {
                    BinCell::Null => Cell::Null,
                    BinCell::Bool(v) => Cell::Bool(v),
                    BinCell::Int(v) => Cell::Int(v),
                    BinCell::Float(v) => Cell::Float(v),
                    BinCell::Text(start, end, true) => Cell::Ascii(&self.buf[start..end]),
                    BinCell::Text(start, end, false) => Cell::Text(&self.buf[start..end]),
                    BinCell::Blob(start, end) => Cell::Blob(&self.buf[start..end]),
                };
                (cell, next)
            }),
        }
    }

    /// Whether another row follows (a live cursor steps to it, once).
    fn row_ahead(&mut self) -> bool {
        match &self.live {
            None => self.c.pos < self.buf.len(),
            Some(cursor) => match self.on_row {
                Some(on) => on,
                None => {
                    // SAFETY: the reader's contract.
                    let on = unsafe { cursor.step() };
                    self.on_row = Some(on);
                    on
                }
            },
        }
    }

    /// The row was read (or skipped): a live cursor steps again when asked for the next.
    fn row_done(&mut self) {
        self.on_row = None;
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
        // SAFETY: in the blob, which lives as long as the buffer or the cursor's row.
        let b = unsafe { *(self.c.blob as *const u8).add(self.c.blob_at) };
        self.c.blob_at += 1;
        self.c.at = At::Bytes;
        Some(b)
    }

    pub fn peek(&mut self) -> u32 {
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
                None => TOKEN_ERROR,
            },
            At::Bytes if self.c.blob_at < self.c.blob_len => TOKEN_NUMBER,
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
            (At::Top, b'[') if !self.single => {
                self.c.at = At::Rows;
                self.row_marks = self.marks.len();
            }
            // A live single row: the cursor steps to it.
            (At::Top, b'{') if self.single && (self.live.is_none() || self.row_ahead()) => {
                self.open_row()
            }
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
        if self.error.is_some() {
            return Err(());
        }
        if self.c.at != At::Keys {
            self.mismatch();
            return Err(());
        }
        if self.c.col >= self.columns {
            self.c.at = if self.single { At::Done } else { At::Rows };
            self.row_done();
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
        if self.error.is_some() {
            return 0;
        }
        match self.c.at {
            At::Top => self.c.at = At::Done,
            At::Row if self.live.is_some() => {
                self.row_done();
                self.c.at = At::Rows;
            }
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
        // A live cursor cannot go back to an earlier row: a decoder that may read the rows
        // again (a union of whole results) gets the ones left in binary form, once.
        if self.live.is_some() && matches!(self.c.at, At::Top | At::Rows) {
            self.buffer_live();
        }
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

    /// Reads the rows a live cursor has left into binary rows and goes on over those.
    fn buffer_live(&mut self) {
        let Some(cursor) = self.live.take() else {
            return;
        };
        let mut w = BinRows::with_names(Vec::new(), &self.buf[1..], self.single);
        // SAFETY: the reader's contract; every `col` is below the column count.
        unsafe {
            let mut on = match (self.c.at, self.on_row) {
                (At::Rows, Some(on)) => on,
                _ => cursor.step(),
            };
            while on {
                for col in 0..self.columns {
                    match cursor.cell(col) {
                        Cell::Null => w.null(),
                        Cell::Bool(v) => w.bool(v),
                        Cell::Int(v) => w.int(v),
                        Cell::Float(v) => w.float(v),
                        Cell::Text(t) | Cell::Ascii(t) => w.text(t),
                        Cell::Blob(b) => w.bytes(b),
                    }
                }
                w.end_row();
                on = !self.single && cursor.step();
            }
        }
        self._owned = w.finish();
        // SAFETY: the bytes live in `_owned`'s heap buffer, which never changes from now on.
        self.buf = unsafe { std::mem::transmute::<&[u8], &'static [u8]>(self._owned.as_slice()) };
        self.on_row = None;
        self.c.pos = bin::header_end(self.buf).map_or(self.buf.len(), |(_, _, start)| start);
    }

    /// The current row (a live cursor's) as a JSON object.
    fn push_live_row(&self, out: &mut Vec<u8>) {
        let mut name = bin::NAMES;
        out.push(b'{');
        for col in 0..self.columns {
            if col > 0 {
                out.push(b',');
            }
            let (start, end, _) = bin::name_at(self.buf, name);
            name = end;
            velt_text::json_escape::push_json_string(out, &self.buf[start..end]);
            out.push(b':');
            if let Some(cursor) = &self.live {
                // SAFETY: the reader's contract; `col` is below the column count.
                bin::push_cell(out, unsafe { cursor.cell(col) });
            }
        }
        out.push(b'}');
    }

    /// One complete value of any kind as a `json.Value` tree: built from the JSON text the
    /// value stands for (a decoder asking for a dynamic value is the rare case).
    pub fn value(&mut self) -> Option<Arc<Value>> {
        if self.error.is_some() {
            return None;
        }
        let mut text = Vec::new();
        let ok = match self.c.at {
            At::Top if self.live.is_some() => {
                if self.single {
                    let on = self.row_ahead();
                    self.push_live_row(&mut text);
                    on
                } else {
                    text.push(b'[');
                    while self.row_ahead() {
                        if text.len() > 1 {
                            text.push(b',');
                        }
                        self.push_live_row(&mut text);
                        self.row_done();
                    }
                    text.push(b']');
                    true
                }
            }
            At::Top => match bin::to_json(self.buf) {
                Some(json) => {
                    text = json;
                    true
                }
                None => false,
            },
            At::Row if self.live.is_some() => {
                self.push_live_row(&mut text);
                true
            }
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

/// An all-ASCII string: one UTF-16 unit per byte.
fn ascii_str(t: &[u8]) -> VeltStr {
    if t.is_empty() {
        return VeltStr::empty();
    }
    // SAFETY: ASCII is UTF-8 with one unit per byte.
    unsafe { VeltStr::from_text_counted(std::str::from_utf8_unchecked(t), t.len()) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use velt_text::rows_cursor::RowCursor;

    thread_local! {
        /// The tests read through a live cursor ([`BufCursor`]) instead of the buffer.
        static LIVE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    /// A reader over `buf`, or over a live cursor serving the same rows.
    fn make(buf: Vec<u8>, flags: u32) -> RowReader {
        if !LIVE.with(|l| l.get()) {
            return RowReader::new(buf, flags);
        }
        let (_, columns, start) = bin::header_end(&buf).unwrap();
        // Leaked: a test's cursor lives as long as the test.
        let cursor = Box::leak(Box::new(BufCursor {
            buf,
            columns,
            start,
            row: None,
        }));
        // SAFETY: the cursor is leaked, so it outlives the reader.
        unsafe { RowReader::live(RawCursor::new(cursor), flags) }
    }

    /// A driver's cursor over binary rows: the reader must answer the same through it.
    struct BufCursor {
        buf: Vec<u8>,
        columns: usize,
        start: usize,
        row: Option<usize>,
    }

    impl RowCursor for BufCursor {
        fn header(&self) -> &[u8] {
            &self.buf[..self.start]
        }
        fn step(&mut self) -> bool {
            let next = match self.row {
                None => self.start,
                Some(mut pos) => {
                    for _ in 0..self.columns {
                        pos = bin::cell_at(&self.buf, pos).unwrap().1;
                    }
                    pos
                }
            };
            self.row = Some(next);
            next < self.buf.len()
        }
        fn cell(&self, col: usize) -> Cell<'_> {
            let mut pos = self.row.unwrap();
            for _ in 0..col {
                pos = bin::cell_at(&self.buf, pos).unwrap().1;
            }
            match bin::cell_at(&self.buf, pos).unwrap().0 {
                BinCell::Null => Cell::Null,
                BinCell::Bool(v) => Cell::Bool(v),
                BinCell::Int(v) => Cell::Int(v),
                BinCell::Float(v) => Cell::Float(v),
                BinCell::Text(a, b, _) => Cell::Text(&self.buf[a..b]),
                BinCell::Blob(a, b) => Cell::Blob(&self.buf[a..b]),
            }
        }
    }

    #[test]
    fn a_live_cursor_reads_as_the_rows_it_serves() {
        LIVE.with(|l| l.set(true));
        reads_rows_as_an_array_of_objects();
        a_single_row_is_an_object();
        no_rows_is_an_empty_array();
        a_wrong_kind_fails_until_reset();
        unknown_keys_are_skipped_or_rejected();
        whole_rows_skip_and_read_as_values();
        negative_zero_reads_as_zero();
        a_column_name_of_any_length_is_kept();
        marks_made_in_a_row_are_forgotten_at_the_next();
        LIVE.with(|l| l.set(false));
    }

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
        let mut r = make(sample(false), 0);
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
        let mut r = make(sample(true), 0);
        assert_eq!(r.peek(), TOKEN_OBJECT_START);
        assert_eq!(r.open(b'['), 0);
        assert_eq!(r.message("array", "$"), "expected array at $");
        let mut r = make(sample(true), 0);
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
        let mut r = make(BinRows::new(["a"], false).finish(), 0);
        assert_eq!(r.open(b'['), 1);
        assert_eq!(r.peek(), TOKEN_ARRAY_END);
        assert_eq!(r.array_next(), STEP_END);
        assert_eq!(r.end(), 1);
    }

    #[test]
    fn a_wrong_kind_fails_until_reset() {
        let mut r = make(sample(false), 0);
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
        let mut r = make(sample(true), 0);
        r.open(b'{');
        key(&mut r);
        assert_eq!(r.skip_unknown(), 1);
        assert_eq!(key(&mut r).as_deref(), Some("name"));
        let mut r = make(sample(true), FLAG_REJECT_UNKNOWN);
        r.open(b'{');
        key(&mut r);
        assert_eq!(r.skip_unknown(), 0);
        assert_eq!(r.message("", "$.id"), "unknown field at $.id");
    }

    #[test]
    fn whole_rows_skip_and_read_as_values() {
        let mut r = make(sample(false), 0);
        r.open(b'[');
        assert_eq!(r.array_next(), STEP_MORE);
        assert_eq!(r.skip(), 1);
        assert_eq!(r.array_next(), STEP_MORE);
        let row = r.value().unwrap();
        assert!(matches!(&*row, Value::Object(o) if o.len() == 6));
        assert_eq!(r.array_next(), STEP_END);
        let mut r = make(sample(false), 0);
        assert!(matches!(&*r.value().unwrap(), Value::Array(rows) if rows.len() == 2));
        assert_eq!(r.end(), 1);
    }

    #[test]
    fn negative_zero_reads_as_zero() {
        // The JSON text these rows stand for writes -0.0 as `0`.
        let mut w = BinRows::new(["x"], true);
        w.float(-0.0);
        w.end_row();
        let buf = w.finish();
        assert_eq!(bin::to_json(&buf).unwrap(), b"{\"x\":0}");
        let mut r = make(buf, 0);
        r.open(b'{');
        key(&mut r);
        assert_eq!(r.f64().map(f64::is_sign_negative), Some(false));
    }

    #[test]
    fn a_column_name_of_any_length_is_kept() {
        let long = format!("{}z", "a".repeat(70_000));
        let mut w = BinRows::new([long.as_str(), "ü".repeat(40_000).as_str()], true);
        w.int(1);
        w.int(2);
        w.end_row();
        let mut r = make(w.finish(), 0);
        r.open(b'{');
        assert_eq!(key(&mut r), Some(long));
        assert_eq!(r.i64(), Some(1));
        assert_eq!(key(&mut r), Some("ü".repeat(40_000)));
        assert_eq!(r.i64(), Some(2));
        assert_eq!(key(&mut r), None);
        assert_eq!(r.end(), 1);
    }

    #[test]
    fn marks_made_in_a_row_are_forgotten_at_the_next() {
        let mut w = BinRows::new(["id"], false);
        for i in 0..1000 {
            w.int(i);
            w.end_row();
        }
        let mut r = make(w.finish(), 0);
        let top = r.mark();
        r.open(b'[');
        for i in 0..1000 {
            assert_eq!(r.array_next(), STEP_MORE);
            // A union decoder looks ahead in each row and reads it again.
            let mark = r.mark();
            r.open(b'{');
            key(&mut r);
            assert!(r.string().is_none());
            r.reset(mark);
            r.open(b'{');
            key(&mut r);
            assert_eq!(r.i64(), Some(i));
            assert_eq!(key(&mut r), None);
            assert!(r.marks.len() <= 2);
        }
        assert_eq!(r.array_next(), STEP_END);
        // A mark made before the rows still reads them again.
        r.reset(top);
        r.open(b'[');
        assert_eq!(r.array_next(), STEP_MORE);
        r.open(b'{');
        key(&mut r);
        assert_eq!(r.i64(), Some(0));
    }

    #[test]
    fn a_damaged_buffer_is_a_mismatch_not_a_crash() {
        let whole = sample(false);
        for cut in 0..whole.len() {
            let mut r = make(whole[..cut].to_vec(), 0);
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
