//! Byte-oriented hand-written lexer: source text → compact token stream + literal payloads.
//!
//! Template literals are split into `Template` tokens (`NoSub`, `Head`, `Middle`, `Tail`) with the
//! substitution expressions lexed as ordinary tokens in between; a brace stack tells a `}` that
//! closes `${` apart from an ordinary `}`. Literal scanning lives in `literals`.

mod literals;
mod regex;
mod token;

pub(crate) use token::{Kw, Payload, Tok, Token, TplPart};

use velt_common::{Diagnostic, FileId, Span};

/// Result of lexing one file.
pub(crate) struct Lexed {
    /// Always ends with an `Eof` token.
    pub toks: Vec<Token>,
    pub payloads: Vec<Payload>,
    pub diags: Vec<Diagnostic>,
}

/// Lexes a whole file. Never fails: problems become diagnostics and lexing continues.
pub(crate) fn lex(file: FileId, src: &str) -> Lexed {
    let mut lx = Lexer {
        src: src.as_bytes(),
        text: src,
        pos: 0,
        file,
        toks: Vec::with_capacity(src.len() / 4 + 1),
        payloads: Vec::new(),
        diags: Vec::new(),
        braces: Vec::new(),
    };
    lx.run();
    Lexed {
        toks: lx.toks,
        payloads: lx.payloads,
        diags: lx.diags,
    }
}

struct Lexer<'a> {
    src: &'a [u8],
    text: &'a str,
    pos: usize,
    file: FileId,
    toks: Vec<Token>,
    payloads: Vec<Payload>,
    diags: Vec<Diagnostic>,
    /// One entry per open `{`; `true` = the brace is a template substitution `${`.
    braces: Vec<bool>,
}

fn is_ident_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_' || c == b'$'
}

fn is_ident_continue(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'$'
}

impl<'a> Lexer<'a> {
    /// Byte at `pos + n`, or 0 past the end (0 never starts a valid token).
    #[inline]
    fn at(&self, n: usize) -> u8 {
        self.src.get(self.pos + n).copied().unwrap_or(0)
    }

    fn error(&mut self, msg: impl Into<String>, lo: usize, hi: usize) {
        let span = Span::new(self.file, lo as u32, hi as u32);
        self.diags.push(Diagnostic::error(msg, span));
    }

    fn push_payload(&mut self, p: Payload) -> u32 {
        self.payloads.push(p);
        (self.payloads.len() - 1) as u32
    }

    fn run(&mut self) {
        if self.src.starts_with(&[0xEF, 0xBB, 0xBF]) {
            self.pos = 3; // UTF-8 BOM
        }
        loop {
            self.skip_trivia();
            let start = self.pos;
            if start >= self.src.len() {
                let end = self.src.len() as u32;
                self.toks.push(Token {
                    kind: Tok::Eof,
                    lo: end,
                    hi: end,
                });
                return;
            }
            if let Some(kind) = self.next_token() {
                self.toks.push(Token {
                    kind,
                    lo: start as u32,
                    hi: self.pos as u32,
                });
            }
        }
    }

    /// Scans one token starting at `self.pos` (not whitespace). `None` = skipped a bad character.
    fn next_token(&mut self) -> Option<Tok> {
        let start = self.pos;
        let c = self.src[start];
        let tok = if is_ident_start(c) {
            self.ident()
        } else if c.is_ascii_digit() || (c == b'.' && self.at(1).is_ascii_digit()) {
            self.number()
        } else if c == b'"' || c == b'\'' {
            self.string(c)
        } else if c == b'`' {
            self.pos += 1;
            self.template(start, true)
        } else if c == b'{' {
            self.pos += 1;
            self.braces.push(false);
            Tok::LBrace
        } else if c == b'}' {
            self.pos += 1;
            if self.braces.pop() == Some(true) {
                self.template(start, false)
            } else {
                Tok::RBrace
            }
        } else if let Some(t) = self.regex_start() {
            t
        } else if let Some(t) = self.punct() {
            t
        } else {
            self.unexpected_char(start);
            return None;
        };
        Some(tok)
    }

    /// Skips the whole UTF-8 scalar so spans stay on char boundaries.
    fn unexpected_char(&mut self, start: usize) {
        let len = self.text[start..]
            .chars()
            .next()
            .map_or(1, |ch| ch.len_utf8());
        self.pos = start + len;
        let shown = self.text[start..self.pos].escape_debug().to_string();
        self.error(format!("unexpected character `{}`", shown), start, self.pos);
    }

    fn skip_trivia(&mut self) {
        loop {
            match self.at(0) {
                b' ' | b'\t' | b'\n' | b'\r' | 0x0B | 0x0C => self.pos += 1,
                b'/' if self.at(1) == b'/' => {
                    while self.pos < self.src.len() && self.src[self.pos] != b'\n' {
                        self.pos += 1;
                    }
                }
                b'/' if self.at(1) == b'*' => self.block_comment(),
                _ => return,
            }
        }
    }

    fn block_comment(&mut self) {
        let start = self.pos;
        self.pos += 2;
        loop {
            if self.pos >= self.src.len() {
                self.error("unterminated block comment", start, start + 2);
                return;
            }
            if self.src[self.pos] == b'*' && self.at(1) == b'/' {
                self.pos += 2;
                return;
            }
            self.pos += 1;
        }
    }

    fn ident(&mut self) -> Tok {
        let start = self.pos;
        while self.pos < self.src.len() && is_ident_continue(self.src[self.pos]) {
            self.pos += 1;
        }
        match Kw::from_word(&self.text[start..self.pos]) {
            Some(k) => Tok::Kw(k),
            None => Tok::Ident,
        }
    }

    /// Operators and punctuation, longest match first.
    fn punct(&mut self) -> Option<Tok> {
        use Tok::*;
        let (tok, len) = match (self.at(0), self.at(1), self.at(2)) {
            (b'(', ..) => (LParen, 1),
            (b')', ..) => (RParen, 1),
            (b'[', ..) => (LBracket, 1),
            (b']', ..) => (RBracket, 1),
            (b';', ..) => (Semi, 1),
            (b',', ..) => (Comma, 1),
            (b':', ..) => (Colon, 1),
            (b'~', ..) => (Tilde, 1),
            (b'.', b'.', b'.') => (DotDotDot, 3),
            (b'.', b'.', b'=') => (DotDotEq, 3),
            (b'.', b'.', _) => (DotDot, 2),
            (b'.', ..) => (Dot, 1),
            (b'?', b'?', b'=') => (QuestionQuestionEq, 3),
            (b'?', b'?', _) => (QuestionQuestion, 2),
            // `a?.5:1` is a ternary, not optional chaining (JS rule).
            (b'?', b'.', c) if !c.is_ascii_digit() => (QuestionDot, 2),
            (b'?', ..) => (Question, 1),
            (b'+', b'+', _) => (PlusPlus, 2),
            (b'+', b'=', _) => (PlusEq, 2),
            (b'+', ..) => (Plus, 1),
            (b'-', b'-', _) => (MinusMinus, 2),
            (b'-', b'=', _) => (MinusEq, 2),
            (b'-', ..) => (Minus, 1),
            (b'*', b'*', b'=') => (StarStarEq, 3),
            (b'*', b'*', _) => (StarStar, 2),
            (b'*', b'=', _) => (StarEq, 2),
            (b'*', ..) => (Star, 1),
            (b'/', b'=', _) => (SlashEq, 2),
            (b'/', ..) => (Slash, 1),
            (b'%', b'=', _) => (PercentEq, 2),
            (b'%', ..) => (Percent, 1),
            (b'<', b'<', b'=') => (ShlEq, 3),
            (b'<', b'<', _) => (Shl, 2),
            (b'<', b'=', _) => (LtEq, 2),
            (b'<', ..) => (Lt, 1),
            (b'>', ..) => (Gt, 1),
            (b'=', b'=', b'=') => (EqEqEq, 3),
            (b'=', b'=', _) => (EqEq, 2),
            (b'=', b'>', _) => (FatArrow, 2),
            (b'=', ..) => (Eq, 1),
            (b'!', b'=', b'=') => (BangEqEq, 3),
            (b'!', b'=', _) => (BangEq, 2),
            (b'!', ..) => (Bang, 1),
            (b'&', b'&', b'=') => (AmpAmpEq, 3),
            (b'&', b'&', _) => (AmpAmp, 2),
            (b'&', b'=', _) => (AmpEq, 2),
            (b'&', ..) => (Amp, 1),
            (b'|', b'|', b'=') => (PipePipeEq, 3),
            (b'|', b'|', _) => (PipePipe, 2),
            (b'|', b'=', _) => (PipeEq, 2),
            (b'|', ..) => (Pipe, 1),
            (b'^', b'=', _) => (CaretEq, 2),
            (b'^', ..) => (Caret, 1),
            _ => return None,
        };
        self.pos += len;
        Some(tok)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<Tok> {
        lex(FileId(0), src)
            .toks
            .into_iter()
            .map(|t| t.kind)
            .collect()
    }

    #[test]
    fn greater_than_is_always_single() {
        use Tok::*;
        assert_eq!(kinds("a>>=b"), vec![Ident, Gt, Gt, Eq, Ident, Eof]);
        assert_eq!(kinds("x=>y"), vec![Ident, FatArrow, Ident, Eof]);
    }

    #[test]
    fn template_tokens_and_brace_stack() {
        let toks = kinds("`a${ {b: 1} }c${d}e`");
        let parts: Vec<_> = toks
            .iter()
            .filter_map(|t| match t {
                Tok::Template(_, part) => Some(*part),
                _ => None,
            })
            .collect();
        assert_eq!(parts, vec![TplPart::Head, TplPart::Middle, TplPart::Tail]);
        assert!(toks.contains(&Tok::LBrace) && toks.contains(&Tok::RBrace));
    }

    #[test]
    fn keywords_and_optional_chaining() {
        use Tok::*;
        assert_eq!(
            kinds("switch a?.b"),
            vec![Kw(super::Kw::Switch), Ident, QuestionDot, Ident, Eof]
        );
        assert_eq!(kinds("a?.5:1")[1], Question);
    }
}
