//! Automatic semicolon insertion's one hazard: a line starting with `(`, `[`, a template
//! literal, `+`, `-` or `/` continues the statement on the line before (`a = b` / `(c)` calls
//! `b`), as in JavaScript. Velt parses it that way, since it is valid TypeScript, and warns, as
//! ESLint's `no-unexpected-multiline` does. The rest of the rules live where they apply:
//! `expect_semi` and `at_inserted_semi` (mod.rs), and the restricted productions at their
//! keywords (`return`, `throw`, `break` / `continue`, `yield`, postfix `++` / `--`, `=>`,
//! `async`).
//!
//! Only a line break at the statement's own level warns: inside parentheses, brackets or braces
//! the line before could not have ended the statement.

use super::Parser;
use crate::lexer::{Tok, TplPart};
use velt_common::{Diagnostic, Severity};

/// How far `Parser::bracket_depth_at` counted last time, so that a statement with many
/// continuation lines is scanned once, not once per line.
#[derive(Clone, Copy, Default)]
pub(super) struct DepthScan {
    start: usize,
    pos: usize,
    depth: i64,
}

/// The enclosing statement's start and depth scan, which [`Parser::end_statement`] restores: a
/// nested statement (a method body in an object literal) must not cost the outer one its scan,
/// or every continuation line of the outer statement would rescan it from its start.
pub(super) struct StmtMark {
    start: usize,
    scan: DepthScan,
}

impl Parser<'_> {
    /// The statement starting at the cursor begins here (for `warn_continuation`); returns the
    /// enclosing statement's state, for [`Self::end_statement`].
    pub(super) fn begin_statement(&mut self) -> StmtMark {
        StmtMark {
            start: std::mem::replace(&mut self.stmt_start, self.pos),
            scan: self.depth_scan,
        }
    }

    /// The statement begun with `mark` ends: the enclosing one's start and scan are back.
    pub(super) fn end_statement(&mut self, mark: StmtMark) {
        self.stmt_start = mark.start;
        self.depth_scan = mark.scan;
    }

    /// The token at the cursor continues an expression although a line break precedes it: warns
    /// if that line break is at the statement's level.
    pub(super) fn warn_continuation(&mut self) {
        if self.speculating > 0
            || !self.line_break_before()
            || !self.line_starts_at_cursor()
            || self.bracket_depth_at() > 0
        {
            return;
        }
        let span = self.cur_span();
        let mut d = Diagnostic::error(
            "this line continues the expression on the line before; add `;` at the end of that line if it is a new statement",
            span,
        );
        d.severity = Severity::Warning;
        self.diags.push(d);
    }

    /// A template literal right after an expression: JavaScript's tagged template, which Velt
    /// does not have. Reported, and the template skipped as an operand of nothing.
    pub(super) fn reject_tagged_template(&mut self) {
        let span = self.cur_span();
        let msg = if self.line_break_before() && self.bracket_depth_at() == 0 {
            "this line continues the expression on the line before as a tagged template, which Velt does not support: add `;` at the end of that line if it is a new statement"
        } else {
            "tagged templates (`` tag`…` ``) are not supported: call a function with the template literal as its argument"
        };
        self.error(msg, span);
    }

    /// Open parentheses, brackets, braces and template substitutions between the start of the
    /// current statement and the cursor.
    fn bracket_depth_at(&mut self) -> i64 {
        let (start, end) = (self.stmt_start, self.pos);
        let scan = self.depth_scan;
        let (mut i, mut depth) = if scan.start == start && scan.pos <= end {
            (scan.pos, scan.depth)
        } else {
            (start, 0)
        };
        crate::work::add(end.saturating_sub(i));
        while i < end {
            depth += match self.tok(i).kind {
                Tok::LParen | Tok::LBracket | Tok::LBrace | Tok::Template(_, TplPart::Head) => 1,
                Tok::RParen | Tok::RBracket | Tok::RBrace | Tok::Template(_, TplPart::Tail) => -1,
                _ => 0,
            };
            i += 1;
        }
        self.depth_scan = DepthScan {
            start,
            pos: end,
            depth,
        };
        depth
    }

    /// Does the cursor's token start a line: a line break between it and the token before,
    /// outside block comments (`a /* … ⏎ … */ + b` continues on the same line)?
    fn line_starts_at_cursor(&mut self) -> bool {
        let lo = self.cur_lo();
        let mut gap = self.text(self.prev_hi.min(lo), lo);
        loop {
            let Some(open) = gap.find("/*") else {
                return has_line_break(gap);
            };
            if has_line_break(&gap[..open]) {
                return true;
            }
            gap = match gap[open + 2..].find("*/") {
                Some(close) => &gap[open + 2 + close + 2..],
                None => "",
            };
        }
    }
}

fn has_line_break(s: &str) -> bool {
    s.contains(['\n', '\r', '\u{2028}', '\u{2029}'])
}
