//! Byte-oriented hand-written lexer: source text → compact token stream + literal payloads.
//!
//! Template literals are split into `Template` tokens (`NoSub`, `Head`, `Middle`, `Tail`) with the
//! substitution expressions lexed as ordinary tokens in between. A mode stack ([`Mode`]) tells a
//! `}` that closes `${` apart from an ordinary `}` and switches between code, JSX tags and JSX
//! children (`jsx`). Literal scanning lives in `literals`, HTML entities in `entities`.
//!
//! The parser pulls tokens on demand ([`Lexer::fill`]). In code a `<` is always `Lt`: only the
//! parser knows whether an expression starts there, so it decides where JSX begins and calls
//! [`Lexer::relex_jsx`], which drops the tokens lexed past that `<` and lexes it again as an
//! element. Each `<` records the mode stack it starts in, so lexing can restart there.

mod entities;
mod jsx;
mod literals;
mod regex;
mod token;

pub(crate) use token::{Kw, Payload, Tok, Token, TplPart};

use velt_common::{Diagnostic, FileId, Span};

/// Result of lexing a whole file as code (no JSX elements outside the parser's control).
#[cfg(test)]
pub(crate) struct Lexed {
    /// Always ends with an `Eof` token.
    pub toks: Vec<Token>,
}

/// How many tokens `Lexer::fill` lexes past the one asked for.
const LEX_BATCH: usize = 32;

/// How many tokens after a `<` that may start an element are lexed one at a time: about as far
/// as the parser looks before deciding whether to re-lex it as JSX (`<div class`, `<p>text`).
const JSX_DECISION_TOKENS: usize = 4;

/// What the lexer is inside of: one entry per open bracket-like construct.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    /// An ordinary `{` in code.
    Brace,
    /// A template substitution `${`: its `}` resumes the template text.
    TemplateSub,
    /// A JSX expression container `{` (attribute value, spread or child).
    JsxExpr,
    /// Inside a JSX tag, after `<` (`closing: false`) or `</` (`closing: true`).
    JsxTag { closing: bool },
    /// Between an opening tag's `>` and the matching `</`.
    JsxChildren,
    /// The type arguments of an opening tag, `<List<number> …>`: code tokens, one mode per open
    /// `<`, whose `>` returns to the tag.
    JsxTypeArgs,
}

/// Lexes a whole file. Never fails: problems become diagnostics and lexing continues.
#[cfg(test)]
pub(crate) fn lex(file: FileId, src: &str) -> Lexed {
    let mut lx = Lexer::new(file, src);
    lx.fill(usize::MAX);
    Lexed { toks: lx.toks }
}

/// The lexer state of one file; `toks` grows as the parser asks for tokens.
pub(crate) struct Lexer<'a> {
    src: &'a [u8],
    text: &'a str,
    pos: usize,
    file: FileId,
    /// Tokens lexed so far; ends with `Eof` once the end is reached.
    pub toks: Vec<Token>,
    pub payloads: Vec<Payload>,
    pub diags: Vec<Diagnostic>,
    /// Mode stacks as a tree: node `i` is `(parent, mode)`; node 0 is the empty stack. Popping
    /// returns to the parent node, so equal ids mean equal stacks.
    modes: Vec<(u32, Mode)>,
    /// The current mode stack (a node of `modes`).
    ctx: u32,
    /// The innermost mode of `ctx` (`None` for the empty stack), kept next to it because every
    /// token reads it.
    mode: Option<Mode>,
    /// Has `Eof` been lexed (the last token)?
    done: bool,
    /// `(token index, mode stack)` of each `<` that may start an element ([`may_start_jsx`]), in
    /// token order: where `relex_jsx` restarts.
    lt_ctx: Vec<(usize, u32)>,
    /// `@jsxImportSource pkg` from a comment before the first token.
    pub jsx_import_source: Option<String>,
    /// Byte ranges of the comments, in source order.
    pub comments: Vec<std::ops::Range<u32>>,
}

/// Can byte `c` start an identifier?
fn is_ident_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_' || c == b'$'
}

/// Can a `<` directly followed by byte `c` start a JSX element (`<name`, or the fragment `<>`)?
/// Only the parser knows whether an expression starts there; for such a `<` the lexer keeps
/// what it needs to re-lex it.
pub(crate) fn may_start_jsx(c: u8) -> bool {
    is_ident_start(c) || c == b'>'
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

    pub(crate) fn new(file: FileId, src: &'a str) -> Self {
        let pos = if src.as_bytes().starts_with(&[0xEF, 0xBB, 0xBF]) {
            3 // UTF-8 BOM
        } else {
            0
        };
        Lexer {
            src: src.as_bytes(),
            text: src,
            pos,
            file,
            // About one token per four bytes of source: one allocation for most files.
            toks: Vec::with_capacity(src.len() / 4 + 1),
            payloads: Vec::new(),
            diags: Vec::new(),
            modes: vec![(0, Mode::Brace)],
            ctx: 0,
            mode: None,
            done: false,
            lt_ctx: Vec::new(),
            jsx_import_source: None,
            comments: Vec::new(),
        }
    }

    /// Lexes until token `i` exists or the file is exhausted (`Eof` lexed). Lexing ahead in
    /// batches is cheaper per token, but the parser may have a `<` that can start an element
    /// re-lexed as JSX, which throws away everything lexed after it: a batch stops at such a
    /// `<`, and near one only what is asked for is lexed.
    pub(crate) fn fill(&mut self, i: usize) {
        if self.done {
            return;
        }
        let near_jsx = self
            .lt_ctx
            .last()
            .is_some_and(|&(j, _)| j + JSX_DECISION_TOKENS >= self.toks.len());
        let end = if near_jsx {
            i
        } else {
            i.saturating_add(LEX_BATCH)
        };
        while self.toks.len() <= end {
            if self.step() && (self.done || self.toks.len() > i) {
                return;
            }
        }
    }

    /// Re-lexes from token `i`, a `<`, as the start of a JSX element: drops token `i` and every
    /// token, diagnostic and comment after it, then restarts in the mode stack of that `<`.
    pub(crate) fn relex_jsx(&mut self, i: usize) {
        let at = self.lt_ctx.partition_point(|&(j, _)| j < i);
        let (Some(&t), Some(&(j, ctx))) = (self.toks.get(i), self.lt_ctx.get(at)) else {
            return;
        };
        if j != i {
            return; // not a `<`
        }
        crate::work::add(self.toks.len() - i);
        self.lt_ctx.truncate(at);
        self.toks.truncate(i);
        // Truncating instead of filtering keeps a file full of errors and elements linear. A
        // diagnostic lies within its token's span or in the trivia before it, so the diagnostics
        // before `t` come first even where one token reports out of order (an unterminated
        // template after an escape inside it).
        // The work counted is what is dropped: filtering would visit every entry instead.
        let keep = self
            .diags
            .partition_point(|d| d.labels.first().is_some_and(|l| l.span.lo < t.lo));
        crate::work::add(self.diags.len() - keep);
        self.diags.truncate(keep);
        let keep = self.comments.partition_point(|c| c.start < t.lo);
        crate::work::add(self.comments.len() - keep);
        self.comments.truncate(keep);
        self.set_ctx(ctx);
        self.done = false;
        self.pos = t.lo as usize + 1;
        self.toks.push(Token {
            kind: Tok::JsxLt,
            lo: t.lo,
            hi: t.lo + 1,
        });
        self.push_mode(Mode::JsxTag { closing: false });
    }

    /// The innermost mode, if any.
    #[inline]
    fn mode(&self) -> Option<Mode> {
        self.mode
    }

    /// Makes node `ctx` of `modes` the current mode stack.
    fn set_ctx(&mut self, ctx: u32) {
        self.ctx = ctx;
        self.mode = (ctx != 0).then(|| self.modes[ctx as usize].1);
    }

    fn push_mode(&mut self, m: Mode) {
        self.modes.push((self.ctx, m));
        self.ctx = (self.modes.len() - 1) as u32;
        self.mode = Some(m);
    }

    fn pop_mode(&mut self) -> Option<Mode> {
        let m = self.mode?;
        self.set_ctx(self.modes[self.ctx as usize].0);
        Some(m)
    }

    /// Lexes one token (skipping bad characters) and appends it. Returns whether a batch of
    /// lexing stops there: at `Eof`, or at a `<` that may start an element.
    /// Inlined into `fill`'s loop, so the lexer state stays in registers from token to token: a
    /// call per token costs about 7% of the lexing time.
    #[inline(always)]
    fn step(&mut self) -> bool {
        loop {
            let mode = self.mode;
            // JSX text is significant: no whitespace or comments are skipped in children.
            if mode != Some(Mode::JsxChildren) {
                self.skip_trivia();
            }
            let start = self.pos;
            if start >= self.src.len() {
                let end = self.src.len() as u32;
                self.toks.push(Token {
                    kind: Tok::Eof,
                    lo: end,
                    hi: end,
                });
                self.done = true;
                return true;
            }
            let kind = match mode {
                Some(Mode::JsxChildren) => Some(self.jsx_children_token()),
                Some(Mode::JsxTag { .. }) => self.jsx_tag_token(),
                Some(Mode::JsxTypeArgs) => self.jsx_type_args_token(),
                _ => self.next_token(),
            };
            if let Some(kind) = kind {
                let jsx_candidate = matches!(kind, Tok::Lt) && may_start_jsx(self.at(0));
                if jsx_candidate {
                    // A `<` leaves the mode stack as it is: lexing restarts in `self.ctx`.
                    self.lt_ctx.push((self.toks.len(), self.ctx));
                }
                self.toks.push(Token {
                    kind,
                    lo: start as u32,
                    hi: self.pos as u32,
                });
                crate::work::add(1);
                return jsx_candidate;
            }
        }
    }

    /// Scans one token starting at `self.pos` (not whitespace). `None` = skipped a bad character.
    fn next_token(&mut self) -> Option<Tok> {
        let start = self.pos;
        let c = self.src[start];
        let tok = if is_ident_start(c) {
            self.ident()
        } else if c == b'#' && is_ident_start(self.at(1)) {
            // `#x`: an ES private name, one token.
            self.pos += 1;
            self.ident();
            Tok::PrivateName
        } else if c.is_ascii_digit() || (c == b'.' && self.at(1).is_ascii_digit()) {
            self.number()
        } else if c == b'"' || c == b'\'' {
            self.string(c)
        } else if c == b'`' {
            self.pos += 1;
            self.template(start, true)
        } else if c == b'{' {
            self.pos += 1;
            self.push_mode(Mode::Brace);
            Tok::LBrace
        } else if c == b'}' {
            self.pos += 1;
            if self.pop_mode() == Some(Mode::TemplateSub) {
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

    /// Inlined into `step` for the same reason.
    #[inline(always)]
    fn skip_trivia(&mut self) {
        loop {
            match self.at(0) {
                b' ' | b'\t' | b'\n' | b'\r' | 0x0B | 0x0C => self.pos += 1,
                b'/' if self.at(1) == b'/' => {
                    let start = self.pos;
                    while self.pos < self.src.len() && self.src[self.pos] != b'\n' {
                        self.pos += 1;
                    }
                    self.comment_done(start);
                }
                b'/' if self.at(1) == b'*' => {
                    let start = self.pos;
                    self.block_comment();
                    self.comment_done(start);
                }
                _ => return,
            }
        }
    }

    /// Records the comment `start..self.pos` (and a leading `@jsxImportSource` pragma).
    fn comment_done(&mut self, start: usize) {
        self.leading_pragma(start);
        self.comments.push(start as u32..self.pos as u32);
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
    fn less_than_in_code_is_never_jsx() {
        use Tok::*;
        assert_eq!(kinds("<div>")[..3], [Lt, Ident, Gt]);
        assert_eq!(
            kinds("return <p />")[..3],
            [Kw(super::Kw::Return), Lt, Ident]
        );
        assert_eq!(kinds("v.as<T>()")[2..5], [Kw(super::Kw::As), Lt, Ident]);
    }

    #[test]
    fn relex_restarts_in_the_mode_of_the_token() {
        use Tok::*;
        // `{` (a template substitution) then `<p>`; re-lexed as JSX, the element closes and
        // the substitution's `}` resumes the template.
        let src = "`a${ <p>it's</p> }b` + 1";
        let mut lx = Lexer::new(FileId(0), src);
        lx.fill(3);
        assert_eq!(lx.toks[1].kind, Lt);
        lx.relex_jsx(1);
        lx.fill(usize::MAX);
        let got: Vec<Tok> = lx.toks.iter().map(|t| t.kind).collect();
        assert!(matches!(got[0], Template(_, TplPart::Head)), "{got:?}");
        assert!(
            matches!(
                got[1..],
                [
                    JsxLt,
                    JsxIdent,
                    JsxGt,
                    JsxText(_),
                    JsxLtSlash,
                    JsxIdent,
                    JsxGt,
                    Template(_, TplPart::Tail),
                    Plus,
                    Int(_),
                    Eof
                ]
            ),
            "{got:?}"
        );
        assert!(lx.diags.is_empty(), "{:?}", lx.diags);
    }

    #[test]
    fn relex_keeps_only_the_diagnostics_before_the_element() {
        // Lexed as code, `'s</p> §` is an unterminated string; as JSX it is text.
        let src = "\u{a7} x = <p>it's</p> \u{a7}";
        let mut lx = Lexer::new(FileId(0), src);
        lx.fill(usize::MAX);
        let messages =
            |lx: &Lexer| -> Vec<String> { lx.diags.iter().map(|d| d.message.clone()).collect() };
        assert!(messages(&lx).contains(&"unterminated string literal".to_string()));
        let lt = lx.toks.iter().position(|t| t.kind == Tok::Lt).unwrap();
        lx.relex_jsx(lt);
        lx.fill(usize::MAX);
        assert_eq!(messages(&lx), ["unexpected character `\u{a7}`"; 2]);
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
