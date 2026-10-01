//! Recursive-descent parser over the token stream. This module holds the parser state and the
//! token/diagnostic primitives; grammar lives in the submodules:
//! `items`/`type_decls`/`members` (declarations), `imports` (imports and re-exports), `stmt`,
//! `expr` (operators), `postfix` (calls, members), `primary` (atoms), `arrow`, `for_loop`,
//! `types`, `patterns`, `undefined` (the rejected `undefined`), and `recovery` (error
//! synchronization).

mod arrow;
mod expr;
mod field_types;
mod for_loop;
mod imports;
mod items;
mod members;
mod param_props;
mod patterns;
mod postfix;
mod primary;
mod recovery;
mod stmt;
mod switch;
mod symbol_keys;
mod type_decls;
mod types;
mod undefined;

use std::collections::HashMap;

use crate::ast::*;
use crate::lexer::{Kw, Lexed, Payload, Tok, Token};
use velt_common::{Diagnostic, FileId, Span};

/// Marker: an error was already reported to `Parser::diags`.
#[derive(Debug)]
pub(crate) struct Fail;
pub(crate) type PResult<T> = Result<T, Fail>;

/// Maximum syntactic nesting depth (expressions, statements, types, patterns) before we bail out
/// with a diagnostic instead of risking a stack overflow. One level costs roughly 6 KB of stack in
/// debug builds; `parse_file` runs the parser on a thread with a large stack (see `lib.rs`).
const MAX_DEPTH: u32 = 256;

/// Parser state for one file.
pub(crate) struct Parser<'a> {
    src: &'a str,
    file: FileId,
    toks: Vec<Token>,
    payloads: Vec<Payload>,
    pos: usize,
    /// End offset of the most recently consumed token.
    prev_hi: u32,
    next_id: u32,
    /// Diagnostics produced so far (lexer diagnostics are merged by the caller).
    pub(crate) diags: Vec<Diagnostic>,
    depth: u32,
    /// Cached decision for `?` tokens (by token index): `true` = ternary, `false` = postfix try.
    ternary_cache: HashMap<usize, bool>,
    /// Set whenever `MAX_DEPTH` is hit, so speculation can tell "too deep" from "doesn't match".
    hit_depth_limit: bool,
    /// Nesting count of speculative parses. While non-zero, diagnostics are suppressed (a failed
    /// attempt would discard them anyway), which keeps backtracking cheap.
    speculating: u32,
    /// Per token index: the index of the matching `)` of a `(` (`NO_MATCH` otherwise). Lets the
    /// parser decide "function type / arrow function?" by looking past the parentheses instead
    /// of speculating, which would re-parse nested parentheses exponentially often.
    paren_close: Vec<u32>,
}

/// `paren_close` entry of a token that is not a matched `(`.
const NO_MATCH: u32 = u32::MAX;

/// Parser position for backtracking (speculative parsing).
#[derive(Clone, Copy)]
struct Snapshot {
    pos: usize,
    prev_hi: u32,
    next_id: u32,
    ndiags: usize,
    depth: u32,
}

impl<'a> Parser<'a> {
    pub(crate) fn new(file: FileId, src: &'a str, lexed: Lexed) -> Self {
        let mut toks = lexed.toks;
        if !matches!(toks.last(), Some(Token { kind: Tok::Eof, .. })) {
            let end = src.len() as u32;
            toks.push(Token {
                kind: Tok::Eof,
                lo: end,
                hi: end,
            });
        }
        Parser {
            src,
            file,
            payloads: lexed.payloads,
            pos: 0,
            prev_hi: 0,
            next_id: 0,
            diags: Vec::new(),
            depth: 0,
            ternary_cache: HashMap::new(),
            hit_depth_limit: false,
            speculating: 0,
            paren_close: match_parens(&toks),
            toks,
        }
    }

    /// The token after the `)` matching the `(` at `pos + off`, if that `(` is matched.
    fn after_matching_paren(&self, off: usize) -> Option<Tok> {
        let close = *self.paren_close.get(self.pos + off)?;
        if close == NO_MATCH {
            return None;
        }
        self.toks.get(close as usize + 1).map(|t| t.kind)
    }

    // ───────────────────────────── token access ─────────────────────────────

    #[inline]
    fn peek(&self) -> Tok {
        self.toks[self.pos].kind
    }

    #[inline]
    fn nth(&self, n: usize) -> Tok {
        let i = (self.pos + n).min(self.toks.len() - 1);
        self.toks[i].kind
    }

    /// Are tokens `pos+n` and `pos+n+1` directly adjacent (no whitespace between)?
    fn adjacent(&self, n: usize) -> bool {
        let i = self.pos + n;
        i + 1 < self.toks.len() && self.toks[i].hi == self.toks[i + 1].lo
    }

    fn cur_lo(&self) -> u32 {
        self.toks[self.pos].lo
    }

    fn cur_span(&self) -> Span {
        let t = &self.toks[self.pos];
        Span::new(self.file, t.lo, t.hi)
    }

    fn bump(&mut self) {
        let t = self.toks[self.pos];
        if t.kind != Tok::Eof {
            self.prev_hi = t.hi;
            self.pos += 1;
        }
    }

    fn at(&self, t: Tok) -> bool {
        self.peek() == t
    }

    fn at_kw(&self, k: Kw) -> bool {
        self.peek() == Tok::Kw(k)
    }

    fn cur_kw(&self) -> Option<Kw> {
        match self.peek() {
            Tok::Kw(k) => Some(k),
            _ => None,
        }
    }

    fn eat(&mut self, t: Tok) -> bool {
        let hit = self.at(t);
        if hit {
            self.bump();
        }
        hit
    }

    fn eat_kw(&mut self, k: Kw) -> bool {
        self.eat(Tok::Kw(k))
    }

    fn expect(&mut self, t: Tok) -> PResult<()> {
        if self.eat(t) {
            return Ok(());
        }
        if self.speculating == 0 {
            self.error_expected(&format!("`{}`", t.describe()));
        }
        Err(Fail)
    }

    fn expect_kw(&mut self, k: Kw, spelling: &str) -> PResult<()> {
        if self.eat_kw(k) {
            return Ok(());
        }
        self.error_expected(&format!("`{}`", spelling));
        Err(Fail)
    }

    fn expect_semi(&mut self) -> PResult<()> {
        self.expect(Tok::Semi)
    }

    /// Cooked text of a `Str`/`Template` token payload.
    fn payload_text(&self, idx: u32) -> String {
        match self.payloads.get(idx as usize) {
            Some(Payload::Text(s)) => s.clone(),
            _ => String::new(),
        }
    }

    // ───────────────────────────── diagnostics ─────────────────────────────

    /// Description of the current token for "found ..." messages.
    fn found(&self) -> String {
        let t = &self.toks[self.pos];
        if t.kind == Tok::Eof {
            return "end of file".to_string();
        }
        let text = self.text(t.lo, t.hi);
        let mut shown: String = text.chars().take(24).collect();
        if shown.len() < text.len() {
            shown.push('…');
        }
        format!("`{}`", shown.replace('\n', "\\n"))
    }

    fn error(&mut self, msg: impl Into<String>, span: Span) {
        if self.speculating == 0 {
            self.diags.push(Diagnostic::error(msg, span));
        }
    }

    /// `expected <what>, found <current token>` at the current token.
    fn error_expected(&mut self, what: &str) {
        if self.speculating > 0 {
            return;
        }
        let msg = format!("expected {}, found {}", what, self.found());
        let span = self.cur_span();
        self.error(msg, span);
    }

    // ───────────────────────────── nodes & spans ─────────────────────────────

    /// Span from `lo` to the end of the last consumed token.
    fn span_from(&self, lo: u32) -> Span {
        Span::new(self.file, lo, self.prev_hi.max(lo))
    }

    fn new_id(&mut self) -> NodeId {
        let id = NodeId(self.next_id);
        self.next_id += 1;
        id
    }

    fn mk_expr(&mut self, kind: ExprKind, span: Span) -> Expr {
        Expr {
            id: self.new_id(),
            kind,
            span,
        }
    }

    fn mk_pat(&mut self, kind: PatternKind, span: Span) -> Pattern {
        Pattern {
            id: self.new_id(),
            kind,
            span,
        }
    }

    // ───────────────────────────── speculation & depth ─────────────────────────────

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            pos: self.pos,
            prev_hi: self.prev_hi,
            next_id: self.next_id,
            ndiags: self.diags.len(),
            depth: self.depth,
        }
    }

    /// Rewinds to `s`, discarding diagnostics and node ids produced since.
    fn restore(&mut self, s: Snapshot) {
        self.pos = s.pos;
        self.prev_hi = s.prev_hi;
        self.next_id = s.next_id;
        self.diags.truncate(s.ndiags);
        self.depth = s.depth;
    }

    /// Tries `f` without committing: on failure the parser is rewound and `None` returned.
    /// Diagnostics are suppressed meanwhile; a successful attempt produces none anyway because the
    /// speculated grammar (types, parameter lists) does no error recovery.
    fn speculate<T>(&mut self, f: impl FnOnce(&mut Self) -> PResult<T>) -> Option<T> {
        let snap = self.snapshot();
        self.speculating += 1;
        let result = f(self);
        self.speculating -= 1;
        match result {
            Ok(v) => Some(v),
            Err(Fail) => {
                self.restore(snap);
                None
            }
        }
    }

    /// Runs `f` one nesting level deeper, failing cleanly if the input nests absurdly deep.
    fn guarded<T>(&mut self, f: impl FnOnce(&mut Self) -> PResult<T>) -> PResult<T> {
        if self.depth >= MAX_DEPTH {
            self.hit_depth_limit = true;
            let span = self.cur_span();
            self.error("expression or statement is nested too deeply", span);
            return Err(Fail);
        }
        self.depth += 1;
        let r = f(self);
        self.depth -= 1;
        r
    }

    // ───────────────────────────── identifiers ─────────────────────────────

    fn text(&self, lo: u32, hi: u32) -> &'a str {
        self.src.get(lo as usize..hi as usize).unwrap_or("")
    }

    /// Identifier or contextual keyword usable as a name.
    fn is_ident_like(t: Tok) -> bool {
        match t {
            Tok::Ident => true,
            Tok::Kw(k) => k.is_soft(),
            _ => false,
        }
    }

    /// Any identifier or keyword (property / member names).
    fn is_name(t: Tok) -> bool {
        matches!(t, Tok::Ident | Tok::Kw(_))
    }

    /// Is the cursor at the plain identifier `word` (a contextual keyword such as `extend`)?
    fn at_word(&self, word: &str) -> bool {
        let t = &self.toks[self.pos];
        t.kind == Tok::Ident && self.text(t.lo, t.hi) == word
    }

    fn at_ident_like(&self) -> bool {
        Self::is_ident_like(self.peek())
    }

    fn parse_ident(&mut self) -> PResult<Ident> {
        if self.at_ident_like() {
            return Ok(self.take_ident());
        }
        self.error_expected("identifier");
        Err(Fail)
    }

    fn parse_prop_name(&mut self) -> PResult<Ident> {
        if Self::is_name(self.peek()) {
            return Ok(self.take_ident());
        }
        self.error_expected("identifier");
        Err(Fail)
    }

    /// Consumes the current token as an identifier (the caller checked its kind).
    fn take_ident(&mut self) -> Ident {
        let t = self.toks[self.pos];
        self.bump();
        Ident {
            name: self.text(t.lo, t.hi).to_string(),
            span: Span::new(self.file, t.lo, t.hi),
        }
    }
}

/// `paren_close` for a token stream: one pass with a stack of open `(` positions.
fn match_parens(toks: &[Token]) -> Vec<u32> {
    let mut close = vec![NO_MATCH; toks.len()];
    let mut open = Vec::new();
    for (i, t) in toks.iter().enumerate() {
        match t.kind {
            Tok::LParen => open.push(i),
            Tok::RParen => {
                if let Some(o) = open.pop() {
                    close[o] = i as u32;
                }
            }
            _ => {}
        }
    }
    close
}
