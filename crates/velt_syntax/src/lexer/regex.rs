//! Regular expression literals (`/ab+c/gi`). A `/` starts one where an operand is expected —
//! after an operator, `(`, `[`, `,`, `=`, `return`… — and is division after an operand (a name,
//! a literal, `)`, `]`, `}`). The body is kept raw (escapes and `[...]` classes, in which `/`
//! does not end the literal); the parser turns it into `new RegExp("ab+c", "gi")` (std/regex).

use super::{Kw, Lexer, Payload, Tok, TplPart};

impl Lexer<'_> {
    /// May a `/` here start a regular expression (no operand just ended)?
    fn regex_allowed(&self) -> bool {
        let Some(prev) = self.toks.last() else {
            return true;
        };
        match prev.kind {
            Tok::Ident
            | Tok::Int(_)
            | Tok::Float(_)
            | Tok::Str(_)
            | Tok::Regex(_)
            | Tok::RParen
            | Tok::RBracket
            | Tok::RBrace
            | Tok::PlusPlus
            | Tok::MinusMinus
            | Tok::Template(_, TplPart::NoSub | TplPart::Tail) => false,
            // A contextual keyword may be a variable (`from / 2`).
            Tok::Kw(k) => !k.is_soft() && !matches!(k, Kw::This | Kw::True | Kw::False | Kw::Null),
            _ => true,
        }
    }

    /// A regular expression literal at `self.pos`, if a `/` starts one here.
    pub(super) fn regex_start(&mut self) -> Option<Tok> {
        let c = self.at(0);
        if c != b'/' || matches!(self.at(1), b'/' | b'*' | b'=') || !self.regex_allowed() {
            return None;
        }
        self.regex()
    }

    /// `/body/flags` at `self.pos` (a `/`); `None` (nothing consumed) when no closing `/`
    /// follows on the same line, so the `/` is lexed as division as before.
    fn regex(&mut self) -> Option<Tok> {
        let start = self.pos;
        let mut i = start + 1;
        let (mut class, mut escaped) = (false, false);
        loop {
            let c = *self.src.get(i)?;
            match c {
                b'\n' | b'\r' => return None,
                _ if escaped => escaped = false,
                b'\\' => escaped = true,
                b'[' => class = true,
                b']' => class = false,
                b'/' if !class => break,
                _ => {}
            }
            i += 1;
        }
        let source = self.text[start + 1..i].to_string();
        i += 1;
        let flags_at = i;
        while self.src.get(i).is_some_and(|c| c.is_ascii_alphabetic()) {
            i += 1;
        }
        let flags = self.text[flags_at..i].to_string();
        self.pos = i;
        Some(Tok::Regex(
            self.push_payload(Payload::Regex { source, flags }),
        ))
    }
}
