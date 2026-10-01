//! Literal scanning: numbers (bases, `_` separators, exponents, type suffixes), strings with
//! escapes, and template-literal pieces.

use super::{is_ident_continue, is_ident_start, Lexer, Mode, Payload, Tok, TplPart};

const INT_SUFFIXES: &[&str] = &[
    "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize",
];
const FLOAT_SUFFIXES: &[&str] = &["f32", "f64"];

fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

impl<'a> Lexer<'a> {
    pub(super) fn number(&mut self) -> Tok {
        let radix = match (self.at(0), self.at(1)) {
            (b'0', b'x' | b'X') => 16,
            (b'0', b'b' | b'B') => 2,
            (b'0', b'o' | b'O') => 8,
            _ => 10,
        };
        let payload = if radix == 10 {
            self.decimal_number()
        } else {
            self.radix_number(radix)
        };
        let idx = self.push_payload(payload);
        match self.payloads[idx as usize] {
            Payload::Float { .. } => Tok::Float(idx),
            _ => Tok::Int(idx),
        }
    }

    /// `0x..`, `0b..`, `0o..` integers.
    fn radix_number(&mut self, radix: u32) -> Payload {
        let start = self.pos;
        self.pos += 2;
        let mut value: u128 = 0;
        let mut overflow = false;
        let mut ndigits = 0;
        loop {
            let c = self.at(0);
            if c == b'_' {
                self.pos += 1;
                continue;
            }
            let digit = match hex_val(c) {
                Some(d) if c.is_ascii_digit() || radix == 16 => d as u32,
                _ => break,
            };
            if digit >= radix {
                self.error(
                    format!("invalid digit `{}` in base-{} literal", c as char, radix),
                    self.pos,
                    self.pos + 1,
                );
            } else {
                ndigits += 1;
                match value
                    .checked_mul(radix as u128)
                    .and_then(|v| v.checked_add(digit as u128))
                {
                    Some(v) => value = v,
                    None => overflow = true,
                }
            }
            self.pos += 1;
        }
        if ndigits == 0 {
            self.error("missing digits after integer base prefix", start, self.pos);
        }
        if overflow {
            self.error("integer literal is too large", start, self.pos);
            value = 0;
        }
        let suffix = match self.suffix() {
            Some((_, s)) if INT_SUFFIXES.contains(&s.as_str()) => Some(s),
            Some((lo, s)) => {
                self.error(
                    format!("invalid suffix `{}` for integer literal", s),
                    lo,
                    self.pos,
                );
                None
            }
            None => None,
        };
        Payload::Int { value, suffix }
    }

    /// Decimal integers and floats: `1_000`, `1.5`, `.5`, `2.5e-3`, `10u8`, `1.0f32`.
    fn decimal_number(&mut self) -> Payload {
        let start = self.pos;
        let mut is_float = false;
        self.digits();
        if self.at(0) == b'.' && self.at(1).is_ascii_digit() {
            is_float = true;
            self.pos += 1;
            self.digits();
        }
        let exp_digit = self.at(1).is_ascii_digit()
            || (matches!(self.at(1), b'+' | b'-') && self.at(2).is_ascii_digit());
        if matches!(self.at(0), b'e' | b'E') && exp_digit {
            is_float = true;
            self.pos += 2;
            self.digits();
        }
        let end = self.pos;
        let mut clean = String::with_capacity(end - start + 1);
        if self.src[start] == b'.' {
            clean.push('0');
        }
        clean.extend(self.text[start..end].chars().filter(|&c| c != '_'));

        let suffix = match self.suffix() {
            Some((_, s)) if FLOAT_SUFFIXES.contains(&s.as_str()) => {
                return Payload::Float {
                    value: clean.parse().unwrap_or(0.0),
                    suffix: Some(s),
                };
            }
            Some((_, s)) if INT_SUFFIXES.contains(&s.as_str()) && !is_float => Some(s),
            Some((lo, s)) => {
                let what = if is_float { "float" } else { "number" };
                self.error(
                    format!("invalid suffix `{}` for {} literal", s, what),
                    lo,
                    self.pos,
                );
                None
            }
            None => None,
        };
        if is_float {
            return Payload::Float {
                value: clean.parse().unwrap_or(0.0),
                suffix,
            };
        }
        let value = clean.bytes().try_fold(0u128, |v, b| {
            v.checked_mul(10)?.checked_add((b - b'0') as u128)
        });
        if value.is_none() {
            self.error("integer literal is too large", start, end);
        }
        Payload::Int {
            value: value.unwrap_or(0),
            suffix,
        }
    }

    fn digits(&mut self) {
        while self.pos < self.src.len()
            && (self.src[self.pos].is_ascii_digit() || self.src[self.pos] == b'_')
        {
            self.pos += 1;
        }
    }

    /// Consumes an alphanumeric word glued to a number (its type suffix, valid or not).
    fn suffix(&mut self) -> Option<(usize, String)> {
        if !is_ident_start(self.at(0)) {
            return None;
        }
        let start = self.pos;
        while self.pos < self.src.len() && is_ident_continue(self.src[self.pos]) {
            self.pos += 1;
        }
        Some((start, self.text[start..self.pos].to_string()))
    }

    pub(super) fn string(&mut self, quote: u8) -> Tok {
        let start = self.pos;
        self.pos += 1;
        let mut out = String::new();
        let mut run = self.pos;
        loop {
            if self.pos >= self.src.len() || matches!(self.src[self.pos], b'\n' | b'\r') {
                out.push_str(&self.text[run..self.pos]);
                self.error("unterminated string literal", start, start + 1);
                break;
            }
            let c = self.src[self.pos];
            if c == quote {
                out.push_str(&self.text[run..self.pos]);
                self.pos += 1;
                break;
            }
            if c == b'\\' {
                out.push_str(&self.text[run..self.pos]);
                self.escape(&mut out, false);
                run = self.pos;
                continue;
            }
            self.pos += 1;
        }
        Tok::Str(self.push_payload(Payload::Text(out)))
    }

    /// `self.pos` is at a backslash; appends the unescaped value. Unknown escapes keep the
    /// backslash (the golden `strings.vlt` prints `\ ` verbatim) and the next character is then
    /// processed normally.
    fn escape(&mut self, out: &mut String, template: bool) {
        let start = self.pos;
        self.pos += 1;
        let c = self.at(0);
        let simple = match c {
            b'n' => Some('\n'),
            b'r' => Some('\r'),
            b't' => Some('\t'),
            b'\\' => Some('\\'),
            b'"' => Some('"'),
            b'\'' => Some('\''),
            b'0' => Some('\0'),
            b'`' | b'$' if template => Some(c as char),
            _ => None,
        };
        if let Some(ch) = simple {
            out.push(ch);
            self.pos += 1;
            return;
        }
        match c {
            b'x' => self.hex_escape(out, start),
            b'u' => self.unicode_escape(out, start),
            _ => out.push('\\'),
        }
    }

    /// `\xHH` (after the backslash).
    fn hex_escape(&mut self, out: &mut String, start: usize) {
        match (hex_val(self.at(1)), hex_val(self.at(2))) {
            (Some(h), Some(l)) => {
                out.push(char::from(h * 16 + l));
                self.pos += 3;
            }
            _ => {
                self.pos += 1;
                self.error(
                    "invalid escape: `\\x` must be followed by two hex digits",
                    start,
                    self.pos,
                );
            }
        }
    }

    /// `\u{H..}` or `\uHHHH` (after the backslash).
    fn unicode_escape(&mut self, out: &mut String, start: usize) {
        self.pos += 1;
        let mut value: u32 = 0;
        let mut ok = true;
        if self.at(0) == b'{' {
            self.pos += 1;
            let mut n = 0;
            while let Some(d) = hex_val(self.at(0)) {
                value = value.saturating_mul(16).saturating_add(d as u32);
                self.pos += 1;
                n += 1;
            }
            ok = self.at(0) == b'}' && (1..=6).contains(&n);
            if self.at(0) == b'}' {
                self.pos += 1;
            }
        } else {
            for _ in 0..4 {
                let Some(d) = hex_val(self.at(0)) else {
                    ok = false;
                    break;
                };
                value = value * 16 + d as u32;
                self.pos += 1;
            }
        }
        match char::from_u32(value) {
            Some(ch) if ok => out.push(ch),
            _ => self.error("invalid unicode escape", start, self.pos),
        }
    }

    /// Scans template text after `` ` `` (`first`) or after the `}` closing a substitution.
    pub(super) fn template(&mut self, start: usize, first: bool) -> Tok {
        let mut out = String::new();
        let mut run = self.pos;
        let end_part = if first { TplPart::NoSub } else { TplPart::Tail };
        let part = loop {
            if self.pos >= self.src.len() {
                out.push_str(&self.text[run..self.pos]);
                self.error("unterminated template literal", start, start + 1);
                break end_part;
            }
            match self.src[self.pos] {
                b'`' => {
                    out.push_str(&self.text[run..self.pos]);
                    self.pos += 1;
                    break end_part;
                }
                b'$' if self.at(1) == b'{' => {
                    out.push_str(&self.text[run..self.pos]);
                    self.pos += 2;
                    self.modes.push(Mode::TemplateSub);
                    break if first {
                        TplPart::Head
                    } else {
                        TplPart::Middle
                    };
                }
                b'\\' => {
                    out.push_str(&self.text[run..self.pos]);
                    self.escape(&mut out, true);
                    run = self.pos;
                }
                b'\r' => {
                    // Template values normalize CRLF / CR to LF, like JS.
                    out.push_str(&self.text[run..self.pos]);
                    out.push('\n');
                    self.pos += if self.at(1) == b'\n' { 2 } else { 1 };
                    run = self.pos;
                }
                _ => self.pos += 1,
            }
        };
        Tok::Template(self.push_payload(Payload::Text(out)), part)
    }
}
