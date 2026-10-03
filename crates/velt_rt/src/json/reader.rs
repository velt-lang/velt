//! Pull reader for compile-time generated `JSON.parse<T>` decoders (`velt_rt_json_reader_*`).
//!
//! The reader borrows the source string (the caller keeps it alive until `reader_free`) and
//! lexes it on demand. The first failure is sticky: every later call fails too, and
//! `velt_rt_json_error` turns it into the `JsonError` message. Commas and colons are handled by
//! `next_key` / `array_next`: the reader only needs to know whether the last consumed token was
//! an opening bracket.

use super::error::{mismatch_message, syntax_message, unknown_message};
use super::scan::{number_f64, number_i64, NumTok, Scanner, StrTok, SyntaxError, TOO_DEEP};
use super::value::{read_limited, Value};
use super::walk::{walk_limited, MemoSink, SkipSink};
use crate::str::VeltStr;
use std::collections::HashMap;
use std::sync::Arc;

/// `peek` results.
pub const TOKEN_EOF: u32 = 0;
pub const TOKEN_NULL: u32 = 1;
pub const TOKEN_TRUE: u32 = 2;
pub const TOKEN_FALSE: u32 = 3;
pub const TOKEN_NUMBER: u32 = 4;
pub const TOKEN_STRING: u32 = 5;
pub const TOKEN_ARRAY_START: u32 = 6;
pub const TOKEN_ARRAY_END: u32 = 7;
pub const TOKEN_OBJECT_START: u32 = 8;
pub const TOKEN_OBJECT_END: u32 = 9;
pub const TOKEN_ERROR: u32 = 10;

/// `next_key` / `array_next` results.
pub const STEP_END: u8 = 0;
pub const STEP_MORE: u8 = 1;
pub const STEP_ERROR: u8 = 2;

/// Why the reader stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadError {
    /// Well-formed so far, but not the kind the decoder asked for.
    Mismatch,
    /// An object key the target type does not have, with unknown keys rejected.
    Unknown,
    Syntax(SyntaxError),
}

/// Opaque `VeltJsonReader*`.
pub struct Reader {
    /// Borrows the caller's string for the reader's lifetime (ABI contract, not checked).
    sc: Scanner<'static>,
    /// The last consumed token was `{` or `[` (so no comma is expected before the next item).
    after_open: bool,
    error: Option<ReadError>,
    /// Options (`JSON.parse` options): fail on unknown object keys; the deepest nesting allowed
    /// (`usize::MAX`: no limit), and the containers open now.
    reject_unknown: bool,
    max_depth: usize,
    depth: usize,
    /// Where each array/object skipped by `skip_lookahead` ends, by the offset of its opening
    /// bracket (created on first use).
    skip_ends: Option<HashMap<usize, usize>>,
    /// Bytes `skip_lookahead` walked (not jumped over): lets tests check that lookahead stays
    /// linear without timing it.
    #[cfg(test)]
    pub(crate) lookahead_walked: usize,
    /// Is the whole source ASCII? Then so is every string read from it, and none needs its
    /// UTF-16 length counted (checked once, a vectorized pass).
    ascii: bool,
}

/// `velt_rt_json_reader_new_with` flags.
pub const FLAG_REJECT_UNKNOWN: u32 = 1;

/// Bits of a mark: position, open containers, `after_open`.
const MARK_DEPTH_BITS: u32 = 21;

impl Reader {
    /// Reader over `src`, which must outlive it.
    pub fn new(src: &'static [u8]) -> Reader {
        Reader::with_options(src, 0, 0)
    }

    /// Reader with `FLAG_*` options and a maximum nesting depth (0: no limit).
    pub fn with_options(src: &'static [u8], flags: u32, max_depth: u32) -> Reader {
        Reader {
            sc: Scanner::new(src),
            after_open: false,
            error: None,
            reject_unknown: flags & FLAG_REJECT_UNKNOWN != 0,
            max_depth: if max_depth == 0 {
                usize::MAX
            } else {
                max_depth as usize
            },
            depth: 0,
            skip_ends: None,
            #[cfg(test)]
            lookahead_walked: 0,
            ascii: src.is_ascii(),
        }
    }

    /// How many more levels a value at the current position may nest.
    fn depth_left(&self) -> usize {
        self.max_depth.saturating_sub(self.depth)
    }

    /// The sticky error, if any.
    pub fn error(&self) -> Option<ReadError> {
        self.error
    }

    fn fail(&mut self, e: ReadError) -> u8 {
        self.error.get_or_insert(e);
        0
    }

    fn syntax(&mut self, r: Result<(), SyntaxError>) -> u8 {
        match r {
            Ok(()) => 1,
            Err(e) => self.fail(ReadError::Syntax(e)),
        }
    }

    /// Start of the next value if it has the wanted first byte; otherwise record a mismatch
    /// (another valid value starts here) or a syntax error.
    fn value_start(&mut self, wanted: impl Fn(u8) -> bool) -> Option<u8> {
        if self.error.is_some() {
            return None;
        }
        self.after_open = false;
        match self.sc.peek_non_ws() {
            Some(b) if wanted(b) => return Some(b),
            Some(b'{' | b'[' | b'"' | b't' | b'f' | b'n' | b'-' | b'0'..=b'9') => {
                self.fail(ReadError::Mismatch)
            }
            _ => self.fail(ReadError::Syntax(self.sc.unexpected())),
        };
        None
    }

    /// Token kind of the next value (see `TOKEN_*`).
    pub fn peek(&mut self) -> u32 {
        if self.error.is_some() {
            return TOKEN_ERROR;
        }
        match self.sc.peek_non_ws() {
            None => TOKEN_EOF,
            Some(b'n') => TOKEN_NULL,
            Some(b't') => TOKEN_TRUE,
            Some(b'f') => TOKEN_FALSE,
            Some(b'-' | b'0'..=b'9') => TOKEN_NUMBER,
            Some(b'"') => TOKEN_STRING,
            Some(b'[') => TOKEN_ARRAY_START,
            Some(b']') => TOKEN_ARRAY_END,
            Some(b'{') => TOKEN_OBJECT_START,
            Some(b'}') => TOKEN_OBJECT_END,
            Some(_) => TOKEN_ERROR,
        }
    }

    /// Consume `{` or `[`.
    pub fn open(&mut self, bracket: u8) -> u8 {
        if self.value_start(|b| b == bracket).is_none() {
            return 0;
        }
        if self.depth >= self.max_depth {
            let e = self.sc.error(TOO_DEEP);
            return self.fail(ReadError::Syntax(e));
        }
        self.sc.pos += 1;
        self.after_open = true;
        self.depth += 1;
        1
    }

    /// Before the next member/element: consume the separating comma or the closing bracket.
    fn step(&mut self, close: u8, expected: &'static str) -> Result<u8, SyntaxError> {
        let first = std::mem::replace(&mut self.after_open, false);
        match self.sc.peek_non_ws() {
            Some(b) if b == close => {
                self.sc.pos += 1;
                self.depth = self.depth.saturating_sub(1);
                Ok(STEP_END)
            }
            Some(b',') if !first => {
                self.sc.pos += 1;
                Ok(STEP_MORE)
            }
            _ if first => Ok(STEP_MORE),
            _ => Err(self.sc.error(expected)),
        }
    }

    fn step_or_fail(&mut self, r: Result<u8, SyntaxError>) -> u8 {
        match r {
            Ok(step) => step,
            Err(e) => {
                self.fail(ReadError::Syntax(e));
                STEP_ERROR
            }
        }
    }

    /// Next object key and its `:`; `Ok(None)` after the closing `}`, `Err` on failure.
    pub(crate) fn next_key(&mut self) -> Result<Option<StrTok>, ()> {
        if self.error.is_some() {
            return Err(());
        }
        let r = self
            .step(b'}', "expected ',' or '}'")
            .and_then(|step| match step {
                STEP_END => Ok(None),
                _ => self.key_colon().map(Some),
            });
        r.map_err(|e| {
            self.fail(ReadError::Syntax(e));
        })
    }

    fn key_colon(&mut self) -> Result<StrTok, SyntaxError> {
        if self.sc.peek_non_ws() != Some(b'"') {
            return Err(self.sc.error("expected string key"));
        }
        let key = self.sc.string(true)?;
        if self.sc.peek_non_ws() != Some(b':') {
            return Err(self.sc.error("expected ':'"));
        }
        self.sc.pos += 1;
        Ok(key)
    }

    /// `STEP_MORE` if another array element follows, `STEP_END` after the closing `]`.
    pub fn array_next(&mut self) -> u8 {
        if self.error.is_some() {
            return STEP_ERROR;
        }
        let step = self.step(b']', "expected ',' or ']'");
        self.step_or_fail(step)
    }

    /// A string value (decoded).
    pub fn string(&mut self) -> Option<StrTok> {
        self.value_start(|b| b == b'"')?;
        match self.sc.string(true) {
            Ok(tok) => Some(tok),
            Err(e) => {
                self.fail(ReadError::Syntax(e));
                None
            }
        }
    }

    /// A number token.
    pub fn number(&mut self) -> Option<NumTok> {
        self.value_start(|b| b == b'-' || b.is_ascii_digit())?;
        match self.sc.number() {
            Ok(tok) => Some(tok),
            Err(e) => {
                self.fail(ReadError::Syntax(e));
                None
            }
        }
    }

    /// A number value as `f64`.
    pub fn f64(&mut self) -> Option<f64> {
        let tok = self.number()?;
        Some(number_f64(self.sc.src, tok))
    }

    /// A number value that is an integer exactly representable as `i64`.
    pub fn i64(&mut self) -> Option<i64> {
        let tok = self.number()?;
        let v = number_i64(self.sc.src, tok);
        if v.is_none() {
            self.fail(ReadError::Mismatch);
        }
        v
    }

    /// `true` / `false`.
    pub fn bool(&mut self) -> Option<bool> {
        let b = self.value_start(|b| b == b't' || b == b'f')?;
        let r = self.sc.literal(if b == b't' { b"true" } else { b"false" });
        (self.syntax(r) == 1).then_some(b == b't')
    }

    /// `null`.
    pub fn null(&mut self) -> u8 {
        if self.value_start(|b| b == b'n').is_none() {
            return 0;
        }
        let r = self.sc.literal(b"null");
        self.syntax(r)
    }

    /// Skip one complete value of any kind (validating it).
    pub fn skip(&mut self) -> u8 {
        if self.value_start(|_| true).is_none() {
            return 0;
        }
        let limit = self.depth_left();
        let r = walk_limited(&mut self.sc, &mut SkipSink, limit);
        self.syntax(r)
    }

    /// Skip one value like `skip`, for a union decoder looking ahead for its discriminant: the
    /// end of every array/object passed is remembered, so the nested unions' own lookahead
    /// jumps over each of them in O(1) (without this, nested unions with the discriminant last
    /// rescan every subtree once per enclosing level: quadratic in the depth).
    pub fn skip_lookahead(&mut self) -> u8 {
        let Some(first) = self.value_start(|_| true) else {
            return 0;
        };
        let limit = self.depth_left();
        #[cfg(test)]
        let start = self.sc.pos;
        if !matches!(first, b'{' | b'[') {
            // A scalar: nothing to remember (one peek, like `skip`).
            let r = walk_limited(&mut self.sc, &mut SkipSink, limit);
            #[cfg(test)]
            {
                self.lookahead_walked += self.sc.pos - start;
            }
            return self.syntax(r);
        }
        let ends = self.skip_ends.get_or_insert_with(HashMap::new);
        // A container at a given offset always has the same depth, so an earlier walk over it
        // already checked the limit.
        if let Some(&end) = ends.get(&self.sc.pos) {
            self.sc.pos = end;
            return 1;
        }
        let mut sink = MemoSink {
            ends,
            open: Vec::new(),
        };
        let r = walk_limited(&mut self.sc, &mut sink, limit);
        #[cfg(test)]
        {
            self.lookahead_walked += self.sc.pos - start;
        }
        self.syntax(r)
    }

    /// Skip the value of an object key the target type does not have (fails when unknown keys
    /// are rejected).
    pub fn skip_unknown(&mut self) -> u8 {
        if self.reject_unknown {
            return self.fail(ReadError::Unknown);
        }
        self.skip()
    }

    /// The current position, to come back to with `reset` (a decoder looking ahead, e.g. for
    /// a union's discriminant).
    pub fn mark(&self) -> u64 {
        let depth = self.depth.min((1 << MARK_DEPTH_BITS) - 1) as u64;
        ((self.sc.pos as u64) << (MARK_DEPTH_BITS + 1)) | (depth << 1) | self.after_open as u64
    }

    /// Go back to `mark` and forget any error since.
    pub fn reset(&mut self, mark: u64) {
        self.sc.pos = (mark >> (MARK_DEPTH_BITS + 1)) as usize;
        self.depth = ((mark >> 1) & ((1 << MARK_DEPTH_BITS) - 1)) as usize;
        self.after_open = mark & 1 == 1;
        self.error = None;
    }

    /// One complete value of any kind, built as a `json.Value` tree.
    pub fn value(&mut self) -> Option<Arc<Value>> {
        self.value_start(|_| true)?;
        let limit = self.depth_left();
        match read_limited(&mut self.sc, limit) {
            Ok(v) => Some(v),
            Err(e) => {
                self.fail(ReadError::Syntax(e));
                None
            }
        }
    }

    /// Only whitespace may follow the top-level value.
    pub fn end(&mut self) -> u8 {
        if self.error.is_some() {
            return 0;
        }
        if self.sc.peek_non_ws().is_some() {
            let e = self.sc.error("unexpected trailing characters");
            return self.fail(ReadError::Syntax(e));
        }
        1
    }

    /// The `JsonError.message` for the current state (see error.rs for the formats).
    pub fn message(&self, expected: &str, path: &str) -> String {
        match self.error {
            Some(ReadError::Syntax(e)) => syntax_message(self.sc.src, e, path, self.max_depth),
            Some(ReadError::Unknown) => unknown_message(path),
            _ => mismatch_message(expected, path),
        }
    }

    /// A string token as a `VeltStr`: borrowed from the source if it had no escapes.
    pub(crate) fn borrowed_str(&self, tok: StrTok) -> VeltStr {
        match tok {
            // SAFETY: the source outlives the reader (reader_new's contract); the static form
            // is never freed.
            StrTok::Borrowed(start, end) => unsafe {
                let units = self.units(&self.sc.src[start..end], true);
                VeltStr::borrowed_text(self.sc.src[start..].as_ptr(), end - start, units)
            },
            StrTok::Owned(v) => self.decoded(&v, false),
        }
    }

    /// A string token as an owned `VeltStr`.
    pub(crate) fn owned_str(&self, tok: StrTok) -> VeltStr {
        match tok {
            StrTok::Borrowed(start, end) if start == end => VeltStr::empty(),
            StrTok::Borrowed(start, end) => self.decoded(&self.sc.src[start..end], true),
            StrTok::Owned(v) => self.decoded(&v, false),
        }
    }

    /// The UTF-16 length of `text`, contents of a string token: its byte length when it is
    /// `raw` (a slice of the source) and the source is ASCII. Decoded contents are counted: an
    /// escape such as `\u00e9` adds a non-ASCII character to ASCII text.
    #[inline]
    fn units(&self, text: &[u8], raw: bool) -> usize {
        if raw && self.ascii {
            text.len()
        } else {
            VeltStr::units_of(text)
        }
    }

    /// The contents of a string token (`raw`: a slice of the source; else decoded, because it had
    /// escapes) as a `VeltStr`.
    #[inline]
    fn decoded(&self, v: &[u8], raw: bool) -> VeltStr {
        // SAFETY: the scanner reads its source as UTF-8 (`scan.rs`): raw contents are a slice of
        // it between two quotes, and decoding turns every escape into a scalar value (a lone
        // surrogate escape becomes U+FFFD), so the text is UTF-8.
        let text = unsafe { std::str::from_utf8_unchecked(v) };
        VeltStr::from_text_counted(text, self.units(v, raw))
    }
}
