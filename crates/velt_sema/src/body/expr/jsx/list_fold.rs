//! Lists in precompiled templates (docs/contracts/jsx.md "Lists"): with the provider's
//! `jsxList`, a child `{xs.map((x) => <tr>…</tr>)}` whose rows are templates without slots is
//! written as `${jsxList(xs.map((x) => `<tr>…</tr>`))}` in the surrounding string. The rows are
//! strings rather than elements, so a list costs what a hand-written one does.
//!
//! Whether a row has slots depends on the types of its children, known only once it is
//! checked. So the child is checked with the row's element marked (`FnCx::jsx_list_fold`):
//! `jsx_template` returns that element's string when it has no slots. Unless that made the
//! child a `string[]` without errors (an array's `map`), everything checked is rolled back and
//! the child is checked as usual.
//!
//! Only an array's `map` is folded, with an arrow that has an expression body and no declared
//! return type, and only an intrinsic element the template can hold (no spread, no `key`).
//! Not:
//! - with a `jsxTextSeparator`: the list is left to the provider (an empty one would make the
//!   text around it adjacent);
//! - after a slot of the same template: the template's strings are evaluated before its slots,
//!   so the rows would run before an earlier component's props;
//! - inside a list being tried: each try may check the rows twice, so nested tries would cost
//!   twice as much per level (the inner list is tried when the outer one is checked as usual).

use velt_common::Span;
use velt_syntax::ast;

use super::intrinsic_tag;
use super::precompile::{precompilable, Template};
use super::provider::{Precompile, Provider};
use crate::body::recheck::Mark;
use crate::body::{FnCx, Want};
use crate::hir::{self, ExprKind as H};

/// The standard arrays' `map` (std/prelude/array.vlt).
const ARRAY_MAP: &str = "std/prelude/array::T[].map";

/// The element of `e` = `xs.map((x) => <el>)` that a fold would make a string, if `e` has that
/// shape.
fn row_element<'e>(p: &Provider, e: &'e ast::Expr) -> Option<&'e ast::JsxElement> {
    let ast::ExprKind::Call {
        callee,
        args,
        optional: false,
        ..
    } = &e.kind
    else {
        return None;
    };
    let ast::ExprKind::Member {
        prop,
        optional: false,
        ..
    } = &callee.kind
    else {
        return None;
    };
    let [arg] = args.as_slice() else { return None };
    let ast::ExprKind::Arrow {
        type_params,
        ret: None,
        body: ast::ArrowBody::Expr(body),
        is_async: false,
        ..
    } = &arg.kind
    else {
        return None;
    };
    if prop.name != "map" || !type_params.is_empty() {
        return None;
    }
    let mut body: &ast::Expr = body;
    while let ast::ExprKind::Paren(inner) = &body.kind {
        body = inner;
    }
    let ast::ExprKind::Jsx(el) = &body.kind else {
        return None;
    };
    let tag = el.name.as_ref().and_then(intrinsic_tag)?;
    (precompilable(p, el, &tag) && !has_visible_slot(el)).then_some(&**el)
}

/// Does `el` hold a component or a fragment somewhere? Then its template has a slot whatever the
/// types are, and a try would only be rolled back.
fn has_visible_slot(el: &ast::JsxElement) -> bool {
    el.children.iter().any(|c| match c {
        ast::JsxChild::Element(inner) => {
            inner.name.as_ref().and_then(intrinsic_tag).is_none() || has_visible_slot(inner)
        }
        _ => false,
    })
}

impl FnCx<'_, '_> {
    /// Writes child `{e}` as a folded list into `t` and returns true, or returns false (nothing
    /// checked) when it isn't one.
    pub(super) fn list_fold(
        &mut self,
        p: &Provider,
        pc: Precompile,
        e: &ast::Expr,
        span: Span,
        t: &mut Template,
    ) -> bool {
        let Some(list) = pc.list else { return false };
        if t.sep.is_some() || !t.slots.is_empty() || self.jsx_list_trial || self.cx.recording() {
            return false;
        }
        let Some(row) = row_element(p, e) else {
            return false;
        };
        let mark = Mark::here(self.cx);
        // `Mark` restores the context; the function's own state (locals, captures, throws,
        // moves) is restored from these copies.
        let frames = (
            self.f.clone(),
            self.outer.clone(),
            self.refused_reads.clone(),
        );
        let diags = self.cx.diags.len();
        let saved = (self.jsx_list_fold.replace(row.span), self.jsx_list_folded);
        self.jsx_list_folded = false;
        self.jsx_list_trial = true;
        let h = self.expr(e, None, Want::Move);
        self.jsx_list_trial = false;
        let folded = self.jsx_list_folded;
        (self.jsx_list_fold, self.jsx_list_folded) = saved;
        let strings = self.cx.ty.array(self.cx.ty.str_);
        if !folded || h.ty != strings || self.cx.diags.len() != diags || !self.is_array_map(&h) {
            mark.rollback(self.cx);
            (self.f, self.outer, self.refused_reads) = frames;
            return false;
        }
        let s = self.jsx_call(p, list, "jsxList", vec![h], span);
        self.add_part(t, s);
        true
    }

    /// Is `h` a call of the standard arrays' `map` (not a `map` of the user's)?
    fn is_array_map(&self, h: &hir::Expr) -> bool {
        let H::Call {
            callee: hir::Callee::Def(d, _),
            ..
        } = &h.kind
        else {
            return false;
        };
        self.cx.fn_info(*d).name == ARRAY_MAP
    }
}
