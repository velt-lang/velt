//! Escape sequences in string and template literals, cooked as ECMAScript does in strict mode
//! (TypeScript modules are strict): `\n \r \t \b \v \f \0`, the quotes and `\\`, `\xHH`,
//! `\uHHHH` (a surrogate pair written as two escapes is one character), `\u{H..}`, a backslash
//! before a line terminator (a line continuation, which adds nothing), and any other character,
//! which stands for itself (`"\d"` is `"d"`). Legacy octal escapes (`\1`, `\01`) and `\8` / `\9`
//! are errors, with tsc's messages and positions.

use super::Lexer;

pub(super) fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

impl<'a> Lexer<'a> {
    /// `self.pos` is at a backslash; appends the cooked value and moves past the escape.
    pub(super) fn escape(&mut self, out: &mut String) {
        let start = self.pos;
        self.pos += 1;
        let c = self.at(0);
        let simple = match c {
            b'n' => '\n',
            b'r' => '\r',
            b't' => '\t',
            b'b' => '\u{8}',
            b'v' => '\u{b}',
            b'f' => '\u{c}',
            b'0' if !self.at(1).is_ascii_digit() => '\0',
            b'x' => return self.hex_escape(out),
            b'u' => return self.unicode_escape(out, start),
            b'0'..=b'7' => return self.octal_escape(out, start),
            b'8' | b'9' => {
                self.error(
                    format!("Escape sequence '\\{}' is not allowed.", c as char),
                    start,
                    self.pos + 1,
                );
                return;
            }
            // Line continuations: `\` + LF, CR, CRLF, U+2028 or U+2029 adds nothing.
            b'\n' => {
                self.pos += 1;
                return;
            }
            b'\r' => {
                self.pos += if self.at(1) == b'\n' { 2 } else { 1 };
                return;
            }
            0xE2 if self.at(1) == 0x80 && matches!(self.at(2), 0xA8 | 0xA9) => {
                self.pos += 3;
                return;
            }
            // `\\`, the quotes, `` \` ``, `\$` and every other character stand for themselves.
            _ => {
                if let Some(ch) = self.text[self.pos..].chars().next() {
                    out.push(ch);
                    self.pos += ch.len_utf8();
                }
                return;
            }
        };
        out.push(simple);
        self.pos += 1;
    }

    /// A legacy octal escape (`\1`, `\01`, `\377`): the value is kept for recovery, but strict
    /// code rejects it as tsc does (TS1487, suggesting the `\x` spelling).
    fn octal_escape(&mut self, out: &mut String, start: usize) {
        let first = self.at(0);
        let max_len = if first <= b'3' { 3 } else { 2 };
        let mut value: u32 = 0;
        let mut len = 0;
        while len < max_len && matches!(self.at(0), b'0'..=b'7') {
            value = value * 8 + (self.at(0) - b'0') as u32;
            self.pos += 1;
            len += 1;
        }
        self.error(
            format!("Octal escape sequences are not allowed. Use the syntax '\\x{value:02x}'."),
            start,
            self.pos,
        );
        out.extend(char::from_u32(value));
    }

    /// `\xHH` (`self.pos` at the `x`).
    fn hex_escape(&mut self, out: &mut String) {
        self.pos += 1;
        let mut value = 0u32;
        for _ in 0..2 {
            let Some(d) = hex_val(self.at(0)) else {
                self.error("Hexadecimal digit expected.", self.pos, self.pos + 1);
                return;
            };
            value = value * 16 + d as u32;
            self.pos += 1;
        }
        out.extend(char::from_u32(value));
    }

    /// `\u{H..}` or `\uHHHH` (`self.pos` at the `u`).
    fn unicode_escape(&mut self, out: &mut String, start: usize) {
        self.pos += 1;
        let value = if self.at(0) == b'{' {
            self.pos += 1;
            let digits = self.pos;
            let mut value: u32 = 0;
            while let Some(d) = hex_val(self.at(0)) {
                value = value.saturating_mul(16).saturating_add(d as u32);
                self.pos += 1;
            }
            if self.pos == digits {
                self.error("Hexadecimal digit expected.", self.pos, self.pos + 1);
                return;
            }
            if value > 0x10FFFF {
                self.error(
                    "An extended Unicode escape value must be between 0x0 and 0x10FFFF inclusive.",
                    digits,
                    self.pos,
                );
                return;
            }
            if self.at(0) != b'}' {
                self.error(
                    "Unterminated Unicode escape sequence.",
                    self.pos,
                    self.pos + 1,
                );
                return;
            }
            self.pos += 1;
            value
        } else {
            match self.hex4() {
                Some(v) => v,
                None => return,
            }
        };
        if (0xD800..0xDC00).contains(&value) {
            // A surrogate pair written as two escapes, in either form (`😀`,
            // `\u{D83D}\u{DE00}`, `\uD83D\u{DE00}`), is one character.
            if let Some((low @ 0xDC00..=0xDFFF, len)) = self.peek_unicode_escape() {
                self.pos += len;
                out.extend(char::from_u32(
                    0x10000 + ((value - 0xD800) << 10) + (low - 0xDC00),
                ));
                return;
            }
        }
        match char::from_u32(value) {
            Some(ch) => out.push(ch),
            None => self.error(
                "a lone surrogate (`\\uD800`–`\\uDFFF` without its pair) is not supported in a \
                 Velt string",
                start,
                self.pos,
            ),
        }
    }

    /// Four hex digits at `self.pos`, consumed; reports tsc's error at the first non-digit.
    fn hex4(&mut self) -> Option<u32> {
        let mut value = 0u32;
        for _ in 0..4 {
            let Some(d) = hex_val(self.at(0)) else {
                self.error("Hexadecimal digit expected.", self.pos, self.pos + 1);
                return None;
            };
            value = value * 16 + d as u32;
            self.pos += 1;
        }
        Some(value)
    }

    /// The value and length of a well-formed `\uHHHH` or `\u{H..}` escape at `self.pos`, not
    /// consumed.
    fn peek_unicode_escape(&self) -> Option<(u32, usize)> {
        if self.at(0) != b'\\' || self.at(1) != b'u' {
            return None;
        }
        if self.at(2) != b'{' {
            let v = (2..6).try_fold(0u32, |v, i| Some(v * 16 + hex_val(self.at(i))? as u32))?;
            return Some((v, 6));
        }
        let mut v: u32 = 0;
        let mut i = 3;
        while let Some(d) = hex_val(self.at(i)) {
            v = v.saturating_mul(16).saturating_add(d as u32);
            i += 1;
        }
        (i > 3 && self.at(i) == b'}').then_some((v, i + 1))
    }
}
