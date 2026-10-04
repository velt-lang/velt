//! A tolerant token scanner over the document text, for features that must work while the code is
//! being typed and does not parse (signature help) or that color every identifier (semantic
//! tokens). It yields identifiers and single punctuation characters with their byte ranges and
//! skips comments, string literals, numbers and the text parts of template literals (their `${}`
//! substitutions are scanned as code). The parser's lexer is private to `velt_syntax`.

/// What a [`Token`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenKind {
    /// An identifier or keyword.
    Ident,
    /// One ASCII punctuation character (multi-character operators come as several tokens).
    Punct(u8),
}

/// One token: its kind and byte range `lo..hi`.
#[derive(Clone, Copy, Debug)]
pub struct Token {
    pub kind: TokenKind,
    pub lo: u32,
    pub hi: u32,
}

/// Tokens of `text[..end]`.
pub fn scan(text: &str, end: usize) -> Vec<Token> {
    let bytes = &text.as_bytes()[..end.min(text.len())];
    let mut s = Scanner {
        bytes,
        pos: 0,
        braces: vec![],
        out: vec![],
        probe: None,
        probed: false,
    };
    s.run();
    s.out
}

/// Whether byte `offset` of `text` is code: not inside a comment, a string literal or the text of
/// a template literal (a position just past a literal's closing quote is code again).
pub fn in_code(text: &str, offset: usize) -> bool {
    let mut s = Scanner {
        bytes: text.as_bytes(),
        pos: 0,
        braces: vec![],
        out: vec![],
        probe: Some(offset),
        probed: false,
    };
    s.run();
    !s.probed
}

struct Scanner<'a> {
    bytes: &'a [u8],
    pos: usize,
    /// For [`in_code`]: the offset asked about, and whether a comment or literal covers it.
    probe: Option<usize>,
    probed: bool,
    /// Open `{`: `true` for a template substitution `${`, whose `}` resumes the template text.
    braces: Vec<bool>,
    out: Vec<Token>,
}

impl Scanner<'_> {
    fn peek(&self, ahead: usize) -> u8 {
        self.bytes.get(self.pos + ahead).copied().unwrap_or(0)
    }

    fn run(&mut self) {
        while self.pos < self.bytes.len() {
            if self.probe.is_some_and(|p| self.pos >= p) {
                return;
            }
            let c = self.peek(0);
            let lo = self.pos;
            match c {
                b'/' if self.peek(1) == b'/' => {
                    self.skip_until(b"\n");
                    let terminated = self.bytes[self.pos - 1] == b'\n';
                    self.covers(lo, !terminated);
                }
                b'/' if self.peek(1) == b'*' => {
                    self.pos += 2;
                    self.skip_until(b"*/");
                    self.covers(lo, false);
                }
                b'"' | b'\'' => {
                    self.string(c);
                    let closed = self.pos > lo + 1 && self.bytes[self.pos - 1] == c;
                    self.covers(lo, !closed);
                }
                b'`' => {
                    self.pos += 1;
                    self.template_text();
                    self.covers(lo, false);
                }
                b'0'..=b'9' => self.skip_while(|b| is_ident_byte(b) || b == b'.'),
                _ if is_ident_start(c) => {
                    let lo = self.pos;
                    self.skip_while(is_ident_byte);
                    self.push(TokenKind::Ident, lo);
                }
                b'{' => {
                    self.braces.push(false);
                    self.punct(c);
                }
                b'}' => {
                    if self.braces.pop() == Some(true) {
                        self.pos += 1;
                        self.template_text();
                        self.covers(lo, false);
                    } else {
                        self.punct(c);
                    }
                }
                _ if c.is_ascii_punctuation() => self.punct(c),
                _ => self.pos += 1,
            }
        }
    }

    /// For [`in_code`]: whether the comment or literal text just skipped (from `lo`) covers the
    /// probed offset; `open_end`: it runs to the end of its line or the text (an unterminated
    /// literal, a line comment), which the offset there is still inside.
    fn covers(&mut self, lo: usize, open_end: bool) {
        if let Some(p) = self.probe {
            if lo < p && (p < self.pos || (open_end && p == self.pos)) {
                self.probed = true;
            }
        }
    }

    fn push(&mut self, kind: TokenKind, lo: usize) {
        self.out.push(Token {
            kind,
            lo: lo as u32,
            hi: self.pos as u32,
        });
    }

    fn punct(&mut self, c: u8) {
        let lo = self.pos;
        self.pos += 1;
        self.push(TokenKind::Punct(c), lo);
    }

    fn skip_while(&mut self, keep: impl Fn(u8) -> bool) {
        while self.pos < self.bytes.len() && keep(self.bytes[self.pos]) {
            self.pos += 1;
        }
    }

    /// Skip past the next `end` (or to the end of the text).
    fn skip_until(&mut self, end: &[u8]) {
        while self.pos < self.bytes.len() && !self.bytes[self.pos..].starts_with(end) {
            self.pos += 1;
        }
        self.pos = (self.pos + end.len()).min(self.bytes.len());
    }

    fn string(&mut self, quote: u8) {
        self.pos += 1;
        while self.pos < self.bytes.len() {
            match self.bytes[self.pos] {
                b'\\' => self.pos += 2,
                b'\n' => return,
                c => {
                    self.pos += 1;
                    if c == quote {
                        return;
                    }
                }
            }
        }
    }

    /// Template text up to the closing backquote, or up to a `${` (which opens a substitution).
    fn template_text(&mut self) {
        while self.pos < self.bytes.len() {
            match self.bytes[self.pos] {
                b'\\' => self.pos += 2,
                b'`' => {
                    self.pos += 1;
                    return;
                }
                b'$' if self.peek(1) == b'{' => {
                    self.pos += 2;
                    self.braces.push(true);
                    return;
                }
                _ => self.pos += 1,
            }
        }
    }
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b == b'$' || b >= 0x80
}

/// Can byte `b` continue an identifier (non-ASCII bytes count, as in names like `größe`)?
pub fn is_ident_byte(b: u8) -> bool {
    is_ident_start(b) || b.is_ascii_digit()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn idents(text: &str) -> Vec<&str> {
        scan(text, text.len())
            .into_iter()
            .filter(|t| t.kind == TokenKind::Ident)
            .map(|t| &text[t.lo as usize..t.hi as usize])
            .collect()
    }

    #[test]
    fn skips_comments_strings_and_numbers() {
        let text = "a /* b */ \"c d\" 'e' // f\n g 1.5f32 h";
        assert_eq!(idents(text), ["a", "g", "h"]);
    }

    #[test]
    fn scans_template_substitutions_as_code() {
        let text = "`x ${ y + { z: 1 }.z } w ${v}` u";
        assert_eq!(idents(text), ["y", "z", "z", "v", "u"]);
    }

    #[test]
    fn tells_code_from_comments_and_literals() {
        let text = "a // b\nc /* d */ \"e\" `f ${g}` 'h";
        let at = |needle: &str| in_code(text, text.find(needle).unwrap());
        assert!(at("a"));
        assert!(!at("b"));
        assert!(at("c"));
        assert!(!at("d"));
        assert!(!at("e"));
        assert!(at(" `"));
        assert!(!at("f"));
        assert!(at("g"));
        assert!(!at("h"));
        assert!(!in_code(text, text.len()));
        assert!(!in_code("x // y", 6));
    }

    #[test]
    fn stops_at_end_offset() {
        let text = "f(a, b";
        let toks = scan(text, 4);
        assert_eq!(toks.len(), 4);
        assert_eq!(toks[3].kind, TokenKind::Punct(b','));
    }
}
