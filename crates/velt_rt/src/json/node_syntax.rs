//! The message Node 22 (V8) gives `JSON.parse` for text that is not JSON, such as
//! `Expected property name or '}' in JSON at position 1 (line 1 column 2)`.
//!
//! Only used once decoding has failed: [`node_syntax_message`] scans the whole text again the
//! way V8's `JsonParser` does (positions, lines and columns count UTF-16 units, as in JS) and
//! reports its first error. A typed decoder that stops at a value of the wrong type reports the
//! syntax error further on instead, since `JSON.parse` in Node reads the whole text first. The
//! nesting depth is not limited here (V8 has no limit).

/// What V8 reports, at a UTF-16 position.
struct Fail {
    pos: usize,
    /// V8's explicit message (`MessageTemplate`), without ` at position …`; `None` picks the
    /// message from the character at `pos` (`LookUpErrorMessageForJsonToken`).
    msg: Option<&'static str>,
}

const PROP_NAME_OR_RBRACE: &str = "Expected property name or '}' in JSON";
const COMMA_OR_RBRACK: &str = "Expected ',' or ']' after array element in JSON";
const COMMA_OR_RBRACE: &str = "Expected ',' or '}' after property value in JSON";
const COLON: &str = "Expected ':' after property name in JSON";
const DOUBLE_QUOTED_NAME: &str = "Expected double-quoted property name in JSON";
const TRAILING: &str = "Unexpected non-whitespace character after JSON";
const UNTERMINATED_STRING: &str = "Unterminated string in JSON";
const BAD_ESCAPE: &str = "Bad escaped character in JSON";
const BAD_CONTROL: &str = "Bad control character in string literal in JSON";
const BAD_UNICODE: &str = "Bad Unicode escape in JSON";
const NO_NUMBER_AFTER_MINUS: &str = "No number after minus sign in JSON";
const UNTERMINATED_FRACTION: &str = "Unterminated fractional number in JSON";
const EXPONENT_MISSING: &str = "Exponent part is missing a number in JSON";

/// Context V8 shows on each side of an unexpected character in a long text.
const CONTEXT: usize = 10;
/// Texts shorter than this many units are quoted whole.
const SHORT_TEXT: usize = 2 * CONTEXT + 1;

/// Node's `SyntaxError` message for `src` (WTF-8), or `None` when it is valid JSON.
pub fn node_syntax_message(src: &[u8]) -> Option<String> {
    let units = utf16_units(src);
    let mut p = Parser { s: &units, i: 0 };
    let fail = p.document().err()?;
    Some(render(&units, fail))
}

/// `src` as UTF-16 code units (a WTF-8 lone surrogate is one unit).
fn utf16_units(src: &[u8]) -> Vec<u16> {
    let mut out = Vec::with_capacity(src.len());
    let mut i = 0;
    while i < src.len() {
        let b = src[i];
        let (len, init) = match b {
            0x00..=0x7F => (1, b as u32),
            0xC0..=0xDF => (2, (b & 0x1F) as u32),
            0xE0..=0xEF => (3, (b & 0x0F) as u32),
            0xF0..=0xF7 => (4, (b & 0x07) as u32),
            _ => (1, 0xFFFD),
        };
        let mut c = init;
        for k in 1..len {
            c = (c << 6) | (src.get(i + k).copied().unwrap_or(0x80) & 0x3F) as u32;
        }
        i += len;
        if c >= 0x10000 {
            let c = c - 0x10000;
            out.push(0xD800 | (c >> 10) as u16);
            out.push(0xDC00 | (c & 0x3FF) as u16);
        } else {
            out.push(c as u16);
        }
    }
    out
}

fn is_digit(c: Option<u16>) -> bool {
    matches!(c, Some(0x30..=0x39))
}

fn is_hex(c: u16) -> bool {
    matches!(c, 0x30..=0x39 | 0x41..=0x46 | 0x61..=0x66)
}

/// A container being read.
enum Cont {
    Array,
    Object,
}

struct Parser<'a> {
    s: &'a [u16],
    i: usize,
}

impl Parser<'_> {
    fn cur(&self) -> Option<u16> {
        self.s.get(self.i).copied()
    }

    fn is(&self, c: u8) -> bool {
        self.cur() == Some(c as u16)
    }

    fn skip_ws(&mut self) {
        while matches!(self.cur(), Some(0x20 | 0x09 | 0x0A | 0x0D)) {
            self.i += 1;
        }
    }

    fn fail<T>(&self, msg: Option<&'static str>) -> Result<T, Fail> {
        Err(Fail { pos: self.i, msg })
    }

    /// Skips whitespace, then consumes `c` or fails with `msg`.
    fn expect(&mut self, c: u8, msg: &'static str) -> Result<(), Fail> {
        self.skip_ws();
        if !self.is(c) {
            return self.fail(Some(msg));
        }
        self.i += 1;
        Ok(())
    }

    /// A property name and its `:` (`msg` when the name is missing).
    fn key(&mut self, msg: &'static str) -> Result<(), Fail> {
        self.expect(b'"', msg)?;
        self.string()?;
        self.expect(b':', COLON)
    }

    /// One value surrounded by whitespace (V8's `ParseJsonValue`, iterative like it).
    fn document(&mut self) -> Result<(), Fail> {
        let mut stack: Vec<Cont> = Vec::new();
        loop {
            // Produce a value; an opened container continues with its first element or key.
            self.skip_ws();
            match self.cur() {
                Some(0x22) => {
                    self.i += 1;
                    self.string()?;
                }
                Some(0x2D | 0x30..=0x39) => self.number()?,
                Some(0x7B) => {
                    self.i += 1;
                    self.skip_ws();
                    if self.is(b'}') {
                        self.i += 1;
                    } else {
                        stack.push(Cont::Object);
                        self.key(PROP_NAME_OR_RBRACE)?;
                        continue;
                    }
                }
                Some(0x5B) => {
                    self.i += 1;
                    self.skip_ws();
                    if self.is(b']') {
                        self.i += 1;
                    } else {
                        stack.push(Cont::Array);
                        continue;
                    }
                }
                Some(0x74) => self.literal(b"true")?,
                Some(0x66) => self.literal(b"false")?,
                Some(0x6E) => self.literal(b"null")?,
                _ => return self.fail(None),
            }
            // The value is done: continue the container it is in.
            loop {
                self.skip_ws();
                match stack.last() {
                    None => {
                        if self.cur().is_some() {
                            return self.fail(Some(TRAILING));
                        }
                        return Ok(());
                    }
                    Some(Cont::Object) => {
                        if self.is(b',') {
                            self.i += 1;
                            self.key(DOUBLE_QUOTED_NAME)?;
                            break;
                        }
                        if !self.is(b'}') {
                            return self.fail(Some(COMMA_OR_RBRACE));
                        }
                    }
                    Some(Cont::Array) => {
                        if self.is(b',') {
                            self.i += 1;
                            break;
                        }
                        if !self.is(b']') {
                            return self.fail(Some(COMMA_OR_RBRACK));
                        }
                    }
                }
                self.i += 1;
                stack.pop();
            }
        }
    }

    /// The rest of a string after its opening quote.
    fn string(&mut self) -> Result<(), Fail> {
        loop {
            let Some(c) = self.cur() else {
                return self.fail(Some(UNTERMINATED_STRING));
            };
            match c {
                0x22 => {
                    self.i += 1;
                    return Ok(());
                }
                0x5C => {
                    self.i += 1;
                    match self.cur() {
                        None => return self.fail(None),
                        Some(0x22 | 0x5C | 0x2F | 0x62 | 0x66 | 0x6E | 0x72 | 0x74) => self.i += 1,
                        Some(0x75) => {
                            self.i += 1;
                            for _ in 0..4 {
                                match self.cur() {
                                    Some(h) if is_hex(h) => self.i += 1,
                                    _ => return self.fail(Some(BAD_UNICODE)),
                                }
                            }
                        }
                        // V8 names only a Latin-1 character as a bad escape.
                        Some(0x100..) => return self.fail(None),
                        Some(_) => return self.fail(Some(BAD_ESCAPE)),
                    }
                }
                0x00..=0x1F => return self.fail(Some(BAD_CONTROL)),
                _ => self.i += 1,
            }
        }
    }

    fn digits(&mut self) {
        while is_digit(self.cur()) {
            self.i += 1;
        }
    }

    fn number(&mut self) -> Result<(), Fail> {
        if self.is(b'-') {
            self.i += 1;
        }
        if self.is(b'0') {
            self.i += 1;
            if is_digit(self.cur()) {
                return self.fail(None);
            }
        } else {
            if !is_digit(self.cur()) {
                return self.fail(Some(NO_NUMBER_AFTER_MINUS));
            }
            self.digits();
        }
        if self.is(b'.') {
            self.i += 1;
            if !is_digit(self.cur()) {
                return self.fail(Some(UNTERMINATED_FRACTION));
            }
            self.digits();
        }
        if self.is(b'e') || self.is(b'E') {
            self.i += 1;
            if self.is(b'+') || self.is(b'-') {
                self.i += 1;
            }
            if !is_digit(self.cur()) {
                return self.fail(Some(EXPONENT_MISSING));
            }
            self.digits();
        }
        Ok(())
    }

    /// `true`, `false` or `null` (the first character already matched): the first character
    /// that differs is unexpected.
    fn literal(&mut self, word: &[u8]) -> Result<(), Fail> {
        self.i += 1;
        for &w in &word[1..] {
            if !self.is(w) {
                return self.fail(None);
            }
            self.i += 1;
        }
        Ok(())
    }
}

/// The text of a V8 `SyntaxError` for `fail` in `s`.
fn render(s: &[u16], fail: Fail) -> String {
    let pos = fail.pos;
    let at = || {
        let (line, column) = line_column(s, pos);
        format!(" at position {pos} (line {line} column {column})")
    };
    if let Some(msg) = fail.msg {
        return format!("{msg}{}", at());
    }
    let Some(&c) = s.get(pos) else {
        return "Unexpected end of JSON input".to_string();
    };
    match c {
        0x2D | 0x30..=0x39 => return format!("Unexpected number in JSON{}", at()),
        0x22 => return format!("Unexpected string in JSON{}", at()),
        _ => {}
    }
    let text = |a: usize, b: usize| String::from_utf16_lossy(&s[a..b.min(s.len())]);
    let whole = text(0, s.len());
    if matches!(
        whole.as_str(),
        "[object Object]" | "undefined" | "Infinity" | "NaN"
    ) {
        return format!("\"{whole}\" is not valid JSON");
    }
    let token = String::from_utf16_lossy(&[c]);
    let len = s.len();
    if len < SHORT_TEXT {
        format!("Unexpected token '{token}', \"{whole}\" is not valid JSON")
    } else if pos < CONTEXT {
        let t = text(0, pos + CONTEXT);
        format!("Unexpected token '{token}', \"{t}\"... is not valid JSON")
    } else if pos < len - CONTEXT {
        let t = text(pos - CONTEXT, pos + CONTEXT);
        format!("Unexpected token '{token}', ...\"{t}\"... is not valid JSON")
    } else {
        let t = text(pos - CONTEXT, len);
        format!("Unexpected token '{token}', ...\"{t}\" is not valid JSON")
    }
}

/// V8's line and column of `pos` (both from 1; `\r\n`, `\r` and `\n` end a line).
fn line_column(s: &[u16], pos: usize) -> (usize, usize) {
    let mut line = 1;
    let mut line_start = 0;
    let mut k = 0;
    while k < pos {
        if s[k] == 0x0D && k + 1 < pos && s[k + 1] == 0x0A {
            k += 1;
        }
        if s[k] == 0x0D || s[k] == 0x0A {
            line += 1;
            line_start = k + 1;
        }
        k += 1;
    }
    (line, 1 + pos - line_start)
}

#[cfg(test)]
mod tests {
    use super::node_syntax_message as msg;

    #[test]
    fn valid_json_has_no_message() {
        for ok in [
            "1",
            " [1, 2e-3, -0.5E+2] ",
            "{\"a\":{\"b\":[]},\"c\":null}",
            "\"\\u00e9\"",
        ] {
            assert_eq!(msg(ok.as_bytes()), None, "{ok}");
        }
    }

    /// Messages read from Node 22.
    #[test]
    fn node_messages() {
        let cases = [
            (
                "{bad",
                "Expected property name or '}' in JSON at position 1 (line 1 column 2)",
            ),
            ("", "Unexpected end of JSON input"),
            ("[1,", "Unexpected end of JSON input"),
            (
                "[1 2]",
                "Expected ',' or ']' after array element in JSON at position 3 (line 1 column 4)",
            ),
            (
                "{\"a\" 1}",
                "Expected ':' after property name in JSON at position 5 (line 1 column 6)",
            ),
            (
                "{\"a\":1,}",
                "Expected double-quoted property name in JSON at position 7 (line 1 column 8)",
            ),
            (
                "\"abc",
                "Unterminated string in JSON at position 4 (line 1 column 5)",
            ),
            (
                "1e",
                "Exponent part is missing a number in JSON at position 2 (line 1 column 3)",
            ),
            (
                "-a",
                "No number after minus sign in JSON at position 1 (line 1 column 2)",
            ),
            (
                "-01",
                "Unexpected number in JSON at position 2 (line 1 column 3)",
            ),
            (
                "1.",
                "Unterminated fractional number in JSON at position 2 (line 1 column 3)",
            ),
            (
                "[1]x",
                "Unexpected non-whitespace character after JSON at position 3 (line 1 column 4)",
            ),
            (
                "\"\\q\"",
                "Bad escaped character in JSON at position 2 (line 1 column 3)",
            ),
            (
                "\"a\tb\"",
                "Bad control character in string literal in JSON at position 2 (line 1 column 3)",
            ),
            (
                "\"\\u12\"",
                "Bad Unicode escape in JSON at position 5 (line 1 column 6)",
            ),
            ("NaN", "\"NaN\" is not valid JSON"),
            ("tru", "Unexpected end of JSON input"),
            ("[tru]", "Unexpected token ']', \"[tru]\" is not valid JSON"),
            (
                "{\"abcdefghijklmnopqrstuvwxyz\": x}",
                "Unexpected token 'x', ...\"tuvwxyz\": x}\" is not valid JSON",
            ),
            (
                "x123456789012345678901234567890",
                "Unexpected token 'x', \"x123456789\"... is not valid JSON",
            ),
            (
                "\n\n  {bad",
                "Expected property name or '}' in JSON at position 5 (line 3 column 4)",
            ),
            (
                "[\"é😀\", x]",
                "Unexpected token 'x', \"[\"é😀\", x]\" is not valid JSON",
            ),
            (
                "{\"key\": \"value\", \"other\": nope, \"more\": 1}",
                "Unexpected token 'o', ...\"\"other\": nope, \"more\"... is not valid JSON",
            ),
            (
                "{\"a\":1}\n\nx",
                "Unexpected non-whitespace character after JSON at position 9 (line 3 column 1)",
            ),
        ];
        for (src, want) in cases {
            assert_eq!(msg(src.as_bytes()).as_deref(), Some(want), "{src:?}");
        }
    }
}
