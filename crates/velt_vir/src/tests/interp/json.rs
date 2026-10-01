//! Emulated JSON pull reader (rt_abi_async.md §12.3, messages §12.4): strict RFC 8259 tokens,
//! commas tracked with a container stack, first failure sticky. Handles are fake pointers.

use std::collections::HashMap;

use super::Interp;

const HANDLE_BASE: u64 = 0x5000_0000;

pub(super) struct Reader {
    src: Vec<u8>,
    pos: usize,
    failed: bool,
    /// First syntax error: (detail, byte offset).
    syntax: Option<(String, usize)>,
    /// Per open container: no element read yet.
    first: Vec<bool>,
}

#[derive(Default)]
pub(super) struct Readers {
    next: u64,
    open: HashMap<u64, Reader>,
}

impl Reader {
    fn ws(&mut self) {
        while self.pos < self.src.len() && b" \t\n\r".contains(&self.src[self.pos]) {
            self.pos += 1;
        }
    }

    fn peek_byte(&mut self) -> Option<u8> {
        self.ws();
        self.src.get(self.pos).copied()
    }

    fn syntax(&mut self, detail: &str) -> u64 {
        self.failed = true;
        if self.syntax.is_none() {
            self.syntax = Some((detail.to_string(), self.pos));
        }
        0
    }

    /// A well-formed value of another kind than asked for.
    fn mismatch(&mut self) -> u64 {
        if self.peek_byte().is_none() {
            return self.syntax("unexpected end of input");
        }
        self.failed = true;
        0
    }

    fn kind(&mut self) -> u32 {
        if self.failed {
            return 10;
        }
        match self.peek_byte() {
            None => 0,
            Some(b'n') => 1,
            Some(b't') => 2,
            Some(b'f') => 3,
            Some(b'-' | b'0'..=b'9') => 4,
            Some(b'"') => 5,
            Some(b'[') => 6,
            Some(b']') => 7,
            Some(b'{') => 8,
            Some(b'}') => 9,
            Some(_) => 10,
        }
    }

    /// A string token at `pos` (which holds `"`).
    fn string(&mut self) -> Option<Vec<u8>> {
        self.pos += 1;
        let mut out = vec![];
        loop {
            let Some(&c) = self.src.get(self.pos) else {
                self.syntax("unexpected end of input");
                return None;
            };
            self.pos += 1;
            match c {
                b'"' => return Some(out),
                b'\\' => {
                    let e = self.src.get(self.pos).copied();
                    self.pos += 1;
                    match e {
                        Some(b'n') => out.push(b'\n'),
                        Some(b't') => out.push(b'\t'),
                        Some(b'r') => out.push(b'\r'),
                        Some(b'b') => out.push(8),
                        Some(b'f') => out.push(12),
                        Some(c @ (b'"' | b'\\' | b'/')) => out.push(c),
                        Some(b'u') => {
                            let hex = self.src.get(self.pos..self.pos + 4).map(|h| h.to_vec());
                            let code = hex
                                .and_then(|h| String::from_utf8(h).ok())
                                .and_then(|h| u32::from_str_radix(&h, 16).ok());
                            let Some(code) = code else {
                                self.syntax("invalid \\u escape");
                                return None;
                            };
                            self.pos += 4;
                            let ch = char::from_u32(code).unwrap_or('\u{fffd}');
                            out.extend(ch.to_string().bytes());
                        }
                        _ => {
                            self.syntax("invalid escape");
                            return None;
                        }
                    }
                }
                c if c < 0x20 => {
                    self.syntax("control character in string");
                    return None;
                }
                c => out.push(c),
            }
        }
    }

    /// A number token at the current position: its text.
    fn number(&mut self) -> Option<String> {
        self.ws();
        let start = self.pos;
        let digits = |r: &mut Reader| {
            let s = r.pos;
            while r.src.get(r.pos).is_some_and(u8::is_ascii_digit) {
                r.pos += 1;
            }
            r.pos > s
        };
        if self.src.get(self.pos) == Some(&b'-') {
            self.pos += 1;
        }
        let mut ok = digits(self);
        if ok && self.src.get(self.pos) == Some(&b'.') {
            self.pos += 1;
            ok = digits(self);
        }
        if ok && matches!(self.src.get(self.pos), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.src.get(self.pos), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            ok = digits(self);
        }
        if !ok {
            self.pos = start;
            return None;
        }
        String::from_utf8(self.src[start..self.pos].to_vec()).ok()
    }

    fn literal(&mut self, lit: &[u8]) -> bool {
        self.ws();
        if self.src[self.pos..].starts_with(lit) {
            self.pos += lit.len();
            true
        } else {
            false
        }
    }

    fn skip(&mut self) -> bool {
        match self.kind() {
            1 => self.literal(b"null"),
            2 => self.literal(b"true"),
            3 => self.literal(b"false"),
            4 => self.number().is_some(),
            5 => self.string().is_some(),
            6 | 8 => {
                let (close, obj) = if self.kind() == 6 {
                    (b']', false)
                } else {
                    (b'}', true)
                };
                self.pos += 1;
                let mut first = true;
                loop {
                    if self.peek_byte() == Some(close) {
                        self.pos += 1;
                        return true;
                    }
                    if !first && self.peek_byte() != Some(b',') {
                        return self.syntax("expected ',' or a closing bracket") == 1;
                    }
                    if !first {
                        self.pos += 1;
                    }
                    first = false;
                    if obj && (self.peek_byte() != Some(b'"') || self.string().is_none()) {
                        return false;
                    }
                    if obj && !self.literal(b":") {
                        return false;
                    }
                    if !self.skip() {
                        return false;
                    }
                }
            }
            _ => self.mismatch() == 1,
        }
    }
}

impl Interp<'_> {
    fn reader(&mut self, h: u64) -> &mut Reader {
        self.exec
            .json
            .open
            .get_mut(&h)
            .expect("interp: unknown JSON reader")
    }

    /// JSON reader functions; `None` if `sym` is not one of them.
    pub(super) fn rt_json(&mut self, sym: &str, a: &[u64]) -> Option<u64> {
        Some(match sym {
            "velt_rt_json_reader_new" => {
                let src = self.str_bytes(a[0]);
                let rs = &mut self.exec.json;
                rs.next += 1;
                let h = HANDLE_BASE + rs.next * 16;
                let r = Reader {
                    src,
                    pos: 0,
                    failed: false,
                    syntax: None,
                    first: vec![],
                };
                rs.open.insert(h, r);
                h
            }
            "velt_rt_json_reader_free" => {
                self.exec.json.open.remove(&a[0]);
                0
            }
            "velt_rt_json_reader_peek" => self.reader(a[0]).kind() as u64,
            "velt_rt_json_reader_expect_object_start" => self.open(a[0], b'{'),
            "velt_rt_json_reader_expect_array_start" => self.open(a[0], b'['),
            "velt_rt_json_reader_next_key" => self.next_key(a[0], a[1]),
            "velt_rt_json_reader_array_next" => self.array_next(a[0]),
            "velt_rt_json_reader_read_string" => {
                let r = self.reader(a[0]);
                if r.kind() != 5 {
                    return Some(r.mismatch());
                }
                match r.string() {
                    Some(s) => {
                        self.new_str(a[1], &s);
                        1
                    }
                    None => 0,
                }
            }
            "velt_rt_json_reader_read_f64" => self.read_number(a[0], a[1], false),
            "velt_rt_json_reader_read_i64" => self.read_number(a[0], a[1], true),
            "velt_rt_json_reader_read_bool" => {
                let r = self.reader(a[0]);
                let v = match r.kind() {
                    2 if r.literal(b"true") => 1u8,
                    3 if r.literal(b"false") => 0,
                    _ => return Some(r.mismatch()),
                };
                self.write_bytes(a[1], &[v]);
                1
            }
            "velt_rt_json_reader_read_null" => {
                let r = self.reader(a[0]);
                if r.kind() == 1 && r.literal(b"null") {
                    1
                } else {
                    r.mismatch()
                }
            }
            "velt_rt_json_reader_skip_value" => {
                let r = self.reader(a[0]);
                (!r.failed && r.skip()) as u64
            }
            "velt_rt_json_reader_end" => {
                let r = self.reader(a[0]);
                if r.peek_byte().is_none() {
                    1
                } else {
                    r.syntax("unexpected trailing characters")
                }
            }
            "velt_rt_json_error" => {
                let expected = self.str_text(a[1]);
                let path = self.str_text(a[2]);
                let msg = match &self.reader(a[0]).syntax {
                    Some((d, off)) => format!("invalid JSON at {path}: {d} (byte {off})"),
                    None => format!("expected {expected} at {path}"),
                };
                self.new_str(a[3], msg.as_bytes());
                0
            }
            _ => return None,
        })
    }

    fn open(&mut self, h: u64, bracket: u8) -> u64 {
        let r = self.reader(h);
        if r.failed || r.peek_byte() != Some(bracket) {
            return if r.failed { 0 } else { r.mismatch() };
        }
        r.pos += 1;
        r.first.push(true);
        1
    }

    /// Before the next element/member: 1 = one follows, 0 = the container closed, 2 = error.
    fn advance(r: &mut Reader, close: u8) -> u64 {
        if r.failed {
            return 2;
        }
        let first = r.first.last().copied().unwrap_or(true);
        match r.peek_byte() {
            Some(c) if c == close => {
                r.pos += 1;
                r.first.pop();
                0
            }
            Some(b',') if !first => {
                r.pos += 1;
                1
            }
            _ if first => 1,
            _ => {
                r.syntax(if close == b'}' {
                    "expected ',' or '}'"
                } else {
                    "expected ',' or ']'"
                });
                2
            }
        }
    }

    fn array_next(&mut self, h: u64) -> u64 {
        let r = self.reader(h);
        let k = Self::advance(r, b']');
        if k == 1 {
            if let Some(f) = r.first.last_mut() {
                *f = false;
            }
        }
        k
    }

    fn next_key(&mut self, h: u64, out: u64) -> u64 {
        let r = self.reader(h);
        let k = Self::advance(r, b'}');
        if k != 1 {
            return k;
        }
        if let Some(f) = r.first.last_mut() {
            *f = false;
        }
        if r.peek_byte() != Some(b'"') {
            r.syntax("expected string key");
            return 2;
        }
        let Some(key) = r.string() else { return 2 };
        if !r.literal(b":") {
            r.syntax("expected ':'");
            return 2;
        }
        self.new_str(out, &key);
        1
    }

    fn read_number(&mut self, h: u64, out: u64, int: bool) -> u64 {
        let r = self.reader(h);
        if r.failed || r.kind() != 4 {
            return if r.failed { 0 } else { r.mismatch() };
        }
        let Some(text) = r.number() else {
            return r.syntax("invalid number");
        };
        let f: f64 = text.parse().unwrap_or(f64::NAN);
        let bits = if !int {
            f.to_bits()
        } else if let Ok(i) = text.parse::<i64>() {
            i as u64
        } else if f.fract() == 0.0 && f.abs() < 9.2e18 {
            f as i64 as u64
        } else {
            r.failed = true;
            return 0;
        };
        self.write_bytes(out, &bits.to_le_bytes());
        1
    }
}
