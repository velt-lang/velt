//! The provider's `jsxTextSeparator` in precompiled templates (docs/contracts/jsx.md "Text
//! separator"). Adjacent text children of one element are collected as a run and written out
//! when markup or a slot ends it. A separator between two parts that are text whatever their
//! values is part of the constant string. A part that may be `null` or a boolean (a boundary,
//! not text) decides at run time: the run's values are bound to temporaries in source order,
//! so each is read once, and the separator next to it is a conditional.
//!
//! Only the provider sees what a slot renders, so it decides the separator between a template
//! string and a slot from the string's edge, where an empty text would be invisible to it. A
//! value that may be the empty string at a slot edge is therefore a slot itself
//! (`Fragment([v], null)`), which the provider renders as in the generic lowering.

use velt_common::Span;

use super::precompile::Template;
use super::provider::{Precompile, Provider};
use crate::body::FnCx;
use crate::hir::{self, ExprKind as H, LogicOp, PatKind as P};

/// A text child in a run.
pub(super) enum TextPart {
    /// Escaped static text.
    Static(String),
    /// A value converted to `JSX.Text`, written with `jsxEscape`, and whether it may be the
    /// empty string.
    Value(hir::Expr, Textness, bool),
}

/// Is a child value text (rather than `true`, `false` or `null`, which are boundaries)?
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Textness {
    Always,
    Never,
    /// Decided at run time: the value's type has a boolean or `null` member and another one.
    Maybe,
}

impl TextPart {
    fn textness(&self) -> Textness {
        match self {
            TextPart::Static(_) => Textness::Always,
            TextPart::Value(_, x, _) => *x,
        }
    }

    fn may_be_empty(&self) -> bool {
        matches!(self, TextPart::Value(_, _, true))
    }
}

/// Before a part of a run being written at run time: `None` for a boundary, `Some(None)` for
/// text, `Some(Some(test))` for text when `test` is true.
type TextTest = Option<Option<hir::Expr>>;

/// A run written at run time: the temporaries binding its values, the parts of its string and
/// the static text not yet added to them.
struct RuntimeRun {
    lets: Vec<hir::Stmt>,
    parts: Vec<hir::Expr>,
    text: String,
}

impl FnCx<'_, '_> {
    /// Whether child value `h` (before its conversion to `JSX.Text`) is text.
    pub(super) fn textness(&mut self, h: &hir::Expr) -> Textness {
        if matches!(h.kind, H::Lit(hir::Lit::Null | hir::Lit::Bool(_))) {
            return Textness::Never;
        }
        let (inner, nullable) = match self.cx.ty.opt_payload(h.ty) {
            Some(inner) => (inner, true),
            None => (h.ty, false),
        };
        let members = self.cx.union_members(inner).unwrap_or_else(|| vec![inner]);
        let bools = members
            .iter()
            .filter(|m| self.cx.typeof_tag(**m) == "boolean")
            .count();
        if bools == members.len() {
            Textness::Never
        } else if nullable || bools > 0 {
            Textness::Maybe
        } else {
            Textness::Always
        }
    }

    /// Whether child value `h` (before its conversion to `JSX.Text`) may be the empty string.
    pub(super) fn may_be_empty(&mut self, h: &hir::Expr) -> bool {
        if let H::Lit(hir::Lit::Str(s)) = &h.kind {
            return s.is_empty();
        }
        let inner = self.cx.ty.opt_payload(h.ty).unwrap_or(h.ty);
        let members = self.cx.union_members(inner).unwrap_or_else(|| vec![inner]);
        members.iter().any(|m| self.cx.typeof_tag(*m) == "string")
    }

    /// Write the pending text run with separators between adjacent text parts; `before_slot`:
    /// a slot follows it. Values that may be empty at a slot edge become slots, in source order.
    pub(super) fn end_run(
        &mut self,
        p: &Provider,
        pc: Precompile,
        t: &mut Template,
        span: Span,
        before_slot: bool,
    ) {
        if t.run.is_empty() {
            return;
        }
        let mut run = std::mem::take(&mut t.run);
        let after_slot = t.text.is_empty() && t.parts.is_empty() && !t.slots.is_empty();
        let head = if after_slot {
            run.iter().take_while(|x| x.may_be_empty()).count()
        } else {
            0
        };
        let mut tail = run.len();
        while before_slot && tail > head && run[tail - 1].may_be_empty() {
            tail -= 1;
        }
        let tail_parts = run.split_off(tail);
        let mid = run.split_off(head);
        for part in run {
            self.text_slot(p, pc, t, part);
        }
        if !mid.is_empty() {
            self.write_run(p, pc, t, mid, span);
        }
        for part in tail_parts {
            self.text_slot(p, pc, t, part);
        }
    }

    /// A text value as the slot `Fragment([v], null)`.
    fn text_slot(&mut self, p: &Provider, pc: Precompile, t: &mut Template, part: TextPart) {
        let TextPart::Value(h, _, _) = part else {
            panic!("ICE: static text as a slot");
        };
        let span = h.span;
        let frag = self.fragment_of(p, h, span);
        self.add_slot(p, pc, t, frag);
    }

    /// Write `run`, whose edges are not empty text next to a slot, into the current string.
    fn write_run(
        &mut self,
        p: &Provider,
        pc: Precompile,
        t: &mut Template,
        run: Vec<TextPart>,
        span: Span,
    ) {
        let sep = t.sep.clone().expect("ICE: a text run without a separator");
        let decided_at_run_time = run.windows(2).any(|w| {
            let (a, b) = (w[0].textness(), w[1].textness());
            a != Textness::Never
                && b != Textness::Never
                && (a, b) != (Textness::Always, Textness::Always)
        });
        if decided_at_run_time {
            self.runtime_run(p, pc, t, run, &sep, span);
        } else {
            self.constant_run(p, pc, t, run, &sep);
        }
    }

    /// Every separator of `run` is known now: constant text between the parts.
    fn constant_run(
        &mut self,
        p: &Provider,
        pc: Precompile,
        t: &mut Template,
        run: Vec<TextPart>,
        sep: &str,
    ) {
        let mut prev = Textness::Never;
        for part in run {
            let cur = part.textness();
            if prev == Textness::Always && cur == Textness::Always {
                t.text.push_str(sep);
            }
            prev = cur;
            match part {
                TextPart::Static(s) => t.text.push_str(&s),
                TextPart::Value(h, _, _) => {
                    let span = h.span;
                    let s = self.escape_call(p, pc, h, span);
                    self.add_part(t, s);
                }
            }
        }
    }

    /// `{ const t1 = v1; …; `…${t1 is text && t2 is text ? sep : ""}…` }`: the values are
    /// read once, in source order.
    fn runtime_run(
        &mut self,
        p: &Provider,
        pc: Precompile,
        t: &mut Template,
        run: Vec<TextPart>,
        sep: &str,
        span: Span,
    ) {
        let mut r = RuntimeRun {
            lets: vec![],
            parts: vec![],
            text: String::new(),
        };
        let mut prev: TextTest = None;
        for part in run {
            let (cur, value) = match part {
                TextPart::Static(s) => (Some(None), Err(s)),
                TextPart::Value(h, x, _) => {
                    let v = self.temp("text", h, &mut r.lets);
                    let cur = match x {
                        Textness::Always => Some(None),
                        Textness::Never => None,
                        Textness::Maybe => Some(Some(self.is_text_test(&v, span))),
                    };
                    (cur, Ok(v))
                }
            };
            self.separator_between(&mut r, prev.take(), &cur, sep, span);
            prev = cur;
            match value {
                Err(s) => r.text.push_str(&s),
                Ok(v) => {
                    self.flush_run_text(&mut r, span);
                    let vspan = v.span;
                    let escaped = self.escape_call(p, pc, v, vspan);
                    r.parts.push(escaped);
                }
            }
        }
        self.flush_run_text(&mut r, span);
        let s = self.concat_parts(r.parts, span);
        let block = self.with_lets(r.lets, s);
        self.add_part(t, block);
    }

    /// The separator between two parts of a run written at run time: constant when both are
    /// text, `test ? sep : ""` when that depends on their values, nothing next to a boundary.
    fn separator_between(
        &mut self,
        r: &mut RuntimeRun,
        prev: TextTest,
        cur: &TextTest,
        sep: &str,
        span: Span,
    ) {
        let (Some(a), Some(b)) = (prev, cur) else {
            return;
        };
        let cond = match (a, b.clone()) {
            (None, None) => return r.text.push_str(sep),
            (Some(a), Some(b)) => self.mk(
                H::Logical {
                    op: LogicOp::And,
                    lhs: Box::new(a),
                    rhs: Box::new(b),
                },
                self.cx.ty.bool_,
                span,
            ),
            (Some(c), None) | (None, Some(c)) => c,
        };
        self.flush_run_text(r, span);
        let kind = H::If {
            cond: Box::new(cond),
            then: Box::new(self.str_lit(sep, span)),
            els: Box::new(self.str_lit("", span)),
        };
        let part = self.mk(kind, self.cx.ty.str_, span);
        r.parts.push(part);
    }

    fn flush_run_text(&mut self, r: &mut RuntimeRun, span: Span) {
        if !r.text.is_empty() {
            let text = std::mem::take(&mut r.text);
            r.parts.push(self.str_lit(&text, span));
        }
    }

    /// Is `v` (a `JSX.Text` temporary) text: not `null` and not a boolean?
    pub(super) fn is_text_test(&mut self, v: &hir::Expr, span: Span) -> hir::Expr {
        let mut alts = vec![];
        for (pat, ty) in self.member_patterns(v.ty, span) {
            if ty.is_some_and(|ty| self.cx.typeof_tag(ty) != "boolean") {
                alts.push(pat);
            }
        }
        let yes = match alts.len() {
            0 => None,
            1 => alts.pop(),
            _ => Some(self.pat(P::Or(alts), v.ty, span)),
        };
        self.bool_match(v.clone(), yes, span)
    }
}
