//! Recursive-descent parser over the token stream. This module holds the parser state and the
//! token/diagnostic primitives; grammar lives in the submodules:
//! `items`/`type_decls`/`members` (declarations), `imports` (imports and re-exports), `stmt`,
//! `expr` (operators), `postfix` (calls, members), `primary` (atoms), `jsx`, `arrow`, `for_loop`,
//! `types`, `patterns`, `undefined` (the rejected `undefined`), `type_assertion` (the rejected
//! `<T>x` of `.ts` files), and `recovery` (error synchronization).

mod arrow;
mod expr;
mod field_types;
mod for_loop;
mod imports;
mod items;
mod jsx;
mod members;
mod param_props;
mod paren_match;
mod patterns;
mod postfix;
mod primary;
mod recovery;
mod script;
mod script_names;
mod stmt;
mod switch;
mod symbol_keys;
mod type_assertion;
mod type_decls;
mod types;
mod undefined;

use std::collections::BTreeMap;

use crate::ast::*;
use crate::lexer::{Kw, Lexer, Payload, Tok, Token};
use paren_match::ParenMatches;
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
    /// Tokens are lexed on demand, so the parser can have a `<` re-lexed as JSX once it knows
    /// that an expression starts there (`relex_jsx`). Looking ahead (`peek`, `nth`) may lex
    /// further, which is why even the lookahead helpers take `&mut self`.
    lx: Lexer<'a>,
    pos: usize,
    /// End offset of the most recently consumed token.
    prev_hi: u32,
    next_id: u32,
    /// Parser diagnostics produced so far (the lexer's are collected by `finish`).
    pub(crate) diags: Vec<Diagnostic>,
    depth: u32,
    /// Cached decision for `?` tokens (by token index). Ordered so that a re-lex drops the
    /// entries from its point on with one `split_off`.
    ternary_cache: BTreeMap<usize, Ternary>,
    /// Number of re-lexes so far: a cached [`Ternary::then_end`] is stale after one.
    relexes: u32,
    /// Set whenever `MAX_DEPTH` is hit, so speculation can tell "too deep" from "doesn't match".
    hit_depth_limit: bool,
    /// Nesting count of speculative parses. While non-zero, diagnostics are suppressed (a failed
    /// attempt would discard them anyway), which keeps backtracking cheap.
    speculating: u32,
    /// Matching parentheses, found on demand (`paren_match`).
    paren_matches: ParenMatches,
    /// A plain `.ts` file: `<T>x` there is TypeScript's type assertion, not JSX
    /// (`type_assertion`).
    pub(crate) plain_ts: bool,
    /// The declarators after the first of `let a = 1, b = 2;` (`parse_var_decl`), for the
    /// statement or item list to add after the first one.
    more_vars: Vec<VarDecl>,
}

/// What the lookahead at a `?` found (`question_is_ternary`).
#[derive(Clone, Copy)]
struct Ternary {
    /// An expression and a `:` follow: a conditional, not the removed postfix `?`.
    is_ternary: bool,
    /// Token index and `prev_hi` where that expression ended. A speculative parse of the
    /// conditional jumps there instead of parsing the branch again (`parse_cond`): otherwise each
    /// enclosing lookahead parses a nested conditional's branch once more, quadratic in the depth.
    then_end: (usize, u32),
    /// [`Parser::relexes`] when it was decided: a re-lex since may have changed the tokens up to
    /// `then_end`.
    relexes: u32,
}

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
    pub(crate) fn new(file: FileId, src: &'a str) -> Self {
        Parser {
            src,
            file,
            lx: Lexer::new(file, src),
            pos: 0,
            prev_hi: 0,
            next_id: 0,
            diags: Vec::new(),
            depth: 0,
            ternary_cache: BTreeMap::new(),
            relexes: 0,
            hit_depth_limit: false,
            speculating: 0,
            paren_matches: ParenMatches::default(),
            plain_ts: false,
            more_vars: Vec::new(),
        }
    }

    /// Lexer diagnostics followed by parser diagnostics, and the comment ranges.
    pub(crate) fn finish(self) -> (Vec<Diagnostic>, Vec<std::ops::Range<u32>>) {
        let lx = self.lx;
        let mut diags = lx.diags;
        diags.extend(self.diags);
        (diags, lx.comments)
    }

    /// Token `i` (the `Eof` token past the end), lexing up to it if needed.
    #[inline]
    fn tok(&mut self, i: usize) -> Token {
        // Lookahead is short, so nearly every access hits a token lexed already.
        if let Some(&t) = self.lx.toks.get(i) {
            return t;
        }
        self.lex_up_to(i)
    }

    /// `tok` for a token not lexed yet: once per batch of tokens, so kept out of line.
    #[cold]
    #[inline(never)]
    fn lex_up_to(&mut self, i: usize) -> Token {
        self.lx.fill(i);
        let last = self.lx.toks.len() - 1;
        self.lx.toks[i.min(last)]
    }

    /// Re-lexes the `<` at the cursor as the start of a JSX element: the parser decided an
    /// expression starts here and it is not a generic arrow. Tokens after the cursor change, so
    /// lookahead caches that saw them are dropped; only those, so a file full of elements still
    /// parses in linear time.
    fn relex_jsx(&mut self) {
        let i = self.pos;
        self.lx.relex_jsx(i);
        self.relexes += 1;
        self.paren_matches.forget_from(i);
        // `split_off` allocates even when nothing moves; usually nothing does.
        if self
            .ternary_cache
            .last_key_value()
            .is_some_and(|(&q, _)| q >= i)
        {
            self.ternary_cache.split_off(&i);
        }
    }

    // ───────────────────────────── token access ─────────────────────────────

    #[inline]
    fn peek(&mut self) -> Tok {
        self.tok(self.pos).kind
    }

    #[inline]
    fn nth(&mut self, n: usize) -> Tok {
        self.tok(self.pos + n).kind
    }

    /// Are tokens `pos+n` and `pos+n+1` directly adjacent (no whitespace between)?
    fn adjacent(&mut self, n: usize) -> bool {
        let a = self.tok(self.pos + n);
        a.kind != Tok::Eof && a.hi == self.tok(self.pos + n + 1).lo
    }

    fn cur_lo(&mut self) -> u32 {
        self.tok(self.pos).lo
    }

    fn cur_span(&mut self) -> Span {
        let t = self.tok(self.pos);
        Span::new(self.file, t.lo, t.hi)
    }

    #[inline]
    fn bump(&mut self) {
        crate::work::add(1);
        let t = self.tok(self.pos);
        if t.kind != Tok::Eof {
            self.prev_hi = t.hi;
            self.pos += 1;
        }
    }

    #[inline]
    fn at(&mut self, t: Tok) -> bool {
        self.peek() == t
    }

    fn at_kw(&mut self, k: Kw) -> bool {
        self.peek() == Tok::Kw(k)
    }

    fn cur_kw(&mut self) -> Option<Kw> {
        match self.peek() {
            Tok::Kw(k) => Some(k),
            _ => None,
        }
    }

    #[inline]
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

    /// The literal payload of a token.
    fn payload(&self, idx: u32) -> Option<&Payload> {
        self.lx.payloads.get(idx as usize)
    }

    /// Cooked text of a `Str`/`Template` token payload.
    fn payload_text(&self, idx: u32) -> String {
        match self.payload(idx) {
            Some(Payload::Text(s)) => s.clone(),
            _ => String::new(),
        }
    }

    // ───────────────────────────── diagnostics ─────────────────────────────

    /// Description of the current token for "found ..." messages.
    fn found(&mut self) -> String {
        let t = self.tok(self.pos);
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
        let msg = if self.at_kw(Kw::Yield) && matches!(what, "identifier" | "pattern") {
            "`yield` is a reserved word and cannot be used as a name (as in TypeScript): choose another name".to_string()
        } else {
            format!("expected {}, found {}", what, self.found())
        };
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
    fn at_word(&mut self, word: &str) -> bool {
        let t = self.tok(self.pos);
        t.kind == Tok::Ident && self.text(t.lo, t.hi) == word
    }

    fn at_ident_like(&mut self) -> bool {
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
        if self.at(Tok::PrivateName) {
            // TS18016; taken, so the rest of the declaration parses.
            let span = self.cur_span();
            self.error("private names are only allowed in class bodies", span);
            return Ok(self.take_ident());
        }
        self.error_expected("identifier");
        Err(Fail)
    }

    /// A property name that may be quoted, as in TypeScript: `name`, a keyword, or a string
    /// literal (`"content-type"`, `"client:load"`) whose text becomes the name. Used where a
    /// field is named (object types, interface fields, destructuring); object literals take
    /// quoted keys themselves (`parse_object_prop`).
    fn parse_prop_key(&mut self) -> PResult<Ident> {
        if let Tok::Str(idx) = self.peek() {
            let key = Ident {
                name: self.payload_text(idx),
                span: self.cur_span(),
            };
            self.bump();
            self.check_quoted_key(&key);
            return Ok(key);
        }
        self.parse_prop_name()
    }

    /// A quoted property name whose text Velt uses for something else: `"#x"` reads as an ES
    /// private name and `"[Symbol.iterator]"` as a symbol key internally, so they are rejected
    /// rather than silently taking that meaning; `"__proto__"` names the prototype in JS.
    fn check_quoted_key(&mut self, key: &Ident) {
        if key.name == "__proto__" {
            self.error(
                "the property name \"__proto__\" is not supported: in JavaScript it sets the object's prototype and creates no property",
                key.span,
            );
        } else if key.name.starts_with(crate::ast::PRIVATE_NAME_PREFIX)
            || key.name.starts_with("[Symbol.")
        {
            self.error(
                format!(
                    "the property name {:?} is not supported: Velt uses names starting with `#` and `[Symbol.` for private names and symbol keys",
                    key.name
                ),
                key.span,
            );
        }
    }

    /// Consumes the current token as an identifier (the caller checked its kind).
    fn take_ident(&mut self) -> Ident {
        let t = self.tok(self.pos);
        self.bump();
        Ident {
            name: self.text(t.lo, t.hi).to_string(),
            span: Span::new(self.file, t.lo, t.hi),
        }
    }
}
