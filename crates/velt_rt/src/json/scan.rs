//! Byte-level JSON lexing (RFC 8259) shared by the pull reader, `skip_value` and the `json.Value`
//! parser. Strings without escapes are returned as ranges of the source (no allocation); only
//! strings with escapes are decoded into a new buffer.

/// A syntax error: what went wrong and the byte offset where it was detected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyntaxError {
    /// Human-readable detail, e.g. [`UNEXPECTED_CHAR`].
    pub what: &'static str,
    /// Byte offset into the source.
    pub at: usize,
}

/// Detail for a byte that cannot start/continue the construct (the message adds the character).
pub const UNEXPECTED_CHAR: &str = "unexpected character";
/// More nested arrays/objects than a decoder's `maxDepth` allows.
pub const TOO_DEEP: &str = "nested too deeply";
/// Detail for input that ends inside a value.
pub const UNEXPECTED_EOF: &str = "unexpected end of input";

/// A lexed string: a byte range of the source (no escapes) or decoded bytes.
#[derive(Debug, PartialEq, Eq)]
pub enum StrTok {
    /// `src[start..end]`, the raw contents between the quotes.
    Borrowed(usize, usize),
    /// Decoded contents (the string had escapes); empty when not decoding.
    Owned(Vec<u8>),
}

impl StrTok {
    /// The string's bytes (decoded).
    pub fn bytes<'a>(&'a self, src: &'a [u8]) -> &'a [u8] {
        match self {
            StrTok::Borrowed(start, end) => &src[*start..*end],
            StrTok::Owned(v) => v,
        }
    }
}

/// A lexed number: `src[start..end]`; `integer` if it has no fraction and no exponent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NumTok {
    pub start: usize,
    pub end: usize,
    pub integer: bool,
}

/// Bytes that end a fast string scan: `"`, `\` and control characters.
static STRING_STOP: [bool; 256] = {
    let mut t = [false; 256];
    let mut i = 0;
    while i < 0x20 {
        t[i] = true;
        i += 1;
    }
    t[b'"' as usize] = true;
    t[b'\\' as usize] = true;
    t
};

/// Cursor over a JSON document.
pub struct Scanner<'a> {
    pub src: &'a [u8],
    pub pos: usize,
}

impl<'a> Scanner<'a> {
    pub fn new(src: &'a [u8]) -> Scanner<'a> {
        Scanner { src, pos: 0 }
    }

    /// Skip whitespace and return the next byte without consuming it (`None` at the end).
    #[inline]
    pub fn peek_non_ws(&mut self) -> Option<u8> {
        while let Some(&b) = self.src.get(self.pos) {
            if !matches!(b, b' ' | b'\n' | b'\r' | b'\t') {
                return Some(b);
            }
            self.pos += 1;
        }
        None
    }

    /// `what` at the current position, or [`UNEXPECTED_EOF`] if the input ended.
    pub fn error(&self, what: &'static str) -> SyntaxError {
        let what = if self.pos >= self.src.len() {
            UNEXPECTED_EOF
        } else {
            what
        };
        SyntaxError { what, at: self.pos }
    }

    /// The current byte cannot appear here.
    pub fn unexpected(&self) -> SyntaxError {
        self.error(UNEXPECTED_CHAR)
    }

    /// Consume exactly `word` (`true`, `false`, `null`).
    pub fn literal(&mut self, word: &[u8]) -> Result<(), SyntaxError> {
        for &want in word {
            if self.src.get(self.pos) != Some(&want) {
                return Err(self.unexpected());
            }
            self.pos += 1;
        }
        Ok(())
    }

    fn digits(&mut self) -> Result<(), SyntaxError> {
        let start = self.pos;
        while self.src.get(self.pos).is_some_and(u8::is_ascii_digit) {
            self.pos += 1;
        }
        if self.pos == start {
            return Err(self.error("invalid number"));
        }
        Ok(())
    }

    /// Lex a number starting at the current position (`-` or a digit).
    pub fn number(&mut self) -> Result<NumTok, SyntaxError> {
        let start = self.pos;
        if self.src.get(self.pos) == Some(&b'-') {
            self.pos += 1;
        }
        match self.src.get(self.pos) {
            Some(b'0') => self.pos += 1,
            Some(b'1'..=b'9') => self.digits()?,
            _ => return Err(self.error("invalid number")),
        }
        let mut integer = true;
        if self.src.get(self.pos) == Some(&b'.') {
            self.pos += 1;
            self.digits()?;
            integer = false;
        }
        if matches!(self.src.get(self.pos), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.src.get(self.pos), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            self.digits()?;
            integer = false;
        }
        Ok(NumTok {
            start,
            end: self.pos,
            integer,
        })
    }

    /// Lex a string whose opening quote is at the current position. With `decode == false`
    /// escapes are validated but not decoded (`Owned` is then empty).
    pub fn string(&mut self, decode: bool) -> Result<StrTok, SyntaxError> {
        self.pos += 1;
        let start = self.pos;
        let mut out: Option<Vec<u8>> = None;
        loop {
            let run = self.pos;
            while self
                .src
                .get(self.pos)
                .is_some_and(|&b| !STRING_STOP[b as usize])
            {
                self.pos += 1;
            }
            match self.src.get(self.pos) {
                Some(b'"') => {
                    let end = self.pos;
                    self.pos += 1;
                    return Ok(match out {
                        None => StrTok::Borrowed(start, end),
                        Some(mut v) => {
                            if decode {
                                v.extend_from_slice(&self.src[run..end]);
                            }
                            StrTok::Owned(v)
                        }
                    });
                }
                Some(b'\\') => {
                    let buf = out.get_or_insert_with(|| {
                        let cap = if decode { self.pos - start + 16 } else { 0 };
                        Vec::with_capacity(cap)
                    });
                    if decode {
                        buf.extend_from_slice(&self.src[run..self.pos]);
                    }
                    self.pos += 1;
                    self.escape(if decode { Some(buf) } else { None })?;
                }
                Some(_) => return Err(self.error("control character in string")),
                None => return Err(self.error(UNEXPECTED_EOF)),
            }
        }
    }

    /// Decode one escape; the backslash is already consumed.
    fn escape(&mut self, out: Option<&mut Vec<u8>>) -> Result<(), SyntaxError> {
        let Some(&c) = self.src.get(self.pos) else {
            return Err(self.error(UNEXPECTED_EOF));
        };
        let decoded = match c {
            b'"' | b'\\' | b'/' => c as char,
            b'b' => '\u{8}',
            b'f' => '\u{c}',
            b'n' => '\n',
            b'r' => '\r',
            b't' => '\t',
            b'u' => {
                self.pos += 1;
                let ch = self.unicode_escape()?;
                if let Some(out) = out {
                    out.extend_from_slice(ch.encode_utf8(&mut [0; 4]).as_bytes());
                }
                return Ok(());
            }
            _ => return Err(self.error("invalid escape")),
        };
        self.pos += 1;
        if let Some(out) = out {
            out.push(decoded as u8);
        }
        Ok(())
    }

    /// `XXXX` after `\u`, combining a following `\uXXXX` low surrogate. Lone surrogates, which
    /// UTF-8 cannot represent, become U+FFFD.
    fn unicode_escape(&mut self) -> Result<char, SyntaxError> {
        let unit = self.hex4()?;
        if !(0xD800..0xDC00).contains(&unit) {
            return Ok(char::from_u32(unit).unwrap_or('\u{FFFD}'));
        }
        if self.src[self.pos..].starts_with(b"\\u") {
            let save = self.pos;
            self.pos += 2;
            let low = self.hex4()?;
            if (0xDC00..0xE000).contains(&low) {
                let cp = 0x10000 + ((unit - 0xD800) << 10) + (low - 0xDC00);
                return Ok(char::from_u32(cp).unwrap_or('\u{FFFD}'));
            }
            // Not a pair: the second escape is decoded on its own.
            self.pos = save;
        }
        Ok('\u{FFFD}')
    }

    fn hex4(&mut self) -> Result<u32, SyntaxError> {
        let mut v = 0u32;
        for _ in 0..4 {
            let digit = self
                .src
                .get(self.pos)
                .and_then(|&b| (b as char).to_digit(16))
                .ok_or_else(|| self.error("invalid \\u escape"))?;
            v = v * 16 + digit;
            self.pos += 1;
        }
        Ok(v)
    }
}

/// The value of a lexed number (correctly rounded, like `JSON.parse`).
pub fn number_f64(src: &[u8], tok: NumTok) -> f64 {
    let text = &src[tok.start..tok.end];
    let (neg, digits) = match text.split_first() {
        Some((b'-', rest)) => (true, rest),
        _ => (false, text),
    };
    if tok.integer && digits.len() <= 15 {
        // Exact: fewer than 2^53.
        let v = digits
            .iter()
            .fold(0u64, |acc, &d| acc * 10 + (d - b'0') as u64) as f64;
        return if neg { -v } else { v };
    }
    // SAFETY: the lexer only accepts ASCII digits, sign, '.', 'e', 'E'.
    let text = unsafe { std::str::from_utf8_unchecked(text) };
    text.parse().unwrap_or(f64::NAN)
}

/// The value of a lexed number as an exact `i64`, or `None` if it is not an integer in range.
pub fn number_i64(src: &[u8], tok: NumTok) -> Option<i64> {
    if !tok.integer {
        let v = number_f64(src, tok);
        // [-2^63, 2^63): both bounds are exact in f64.
        let limit = -(i64::MIN as f64);
        let in_range = (-limit..limit).contains(&v);
        return (v.fract() == 0.0 && in_range).then_some(v as i64);
    }
    let text = &src[tok.start..tok.end];
    let (neg, digits) = match text.split_first() {
        Some((b'-', rest)) => (true, rest),
        _ => (false, text),
    };
    // Accumulate negatively so i64::MIN fits.
    let mut acc: i64 = 0;
    for &d in digits {
        acc = acc.checked_mul(10)?.checked_sub((d - b'0') as i64)?;
    }
    if neg {
        Some(acc)
    } else {
        acc.checked_neg()
    }
}
