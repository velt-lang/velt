//! Reads of a group of std's `RegExpMatch` that JavaScript's match array gives as `undefined`
//! when the group did not take part (#3562): its typed `m[n]` is a `string` (`""` there), so the
//! forms that observe the difference test it instead. `m[n] === undefined`, `== null` and
//! `?? d` are `never_null.rs`'s element test; `m?.[n]` is here.

use velt_common::Span;
use velt_syntax::ast;

use crate::body::{FnCx, Want};
use crate::hir::{self, TyId};

/// std's match class.
const REGEXP_MATCH: &str = velt_common::std_item!(text_regex_regexp_match, "RegExpMatch");

impl FnCx<'_, '_> {
    /// Is `t` std's `RegExpMatch`, or a type parameter bounded by it (`T extends RegExpMatch`)?
    pub(super) fn is_regexp_match(&mut self, t: TyId) -> bool {
        let t = self.class_bound(t).unwrap_or(t);
        self.is_std_class(t, REGEXP_MATCH)
    }

    /// `m?.[n]` on a match `v` (the chain's present value) with `n` a whole number literal:
    /// `v.at(n)`, `undefined` for a group that did not take part or does not exist, as in
    /// JavaScript. (A computed `n` reads `v[n]`: `at` counts a negative one from the end.)
    pub(super) fn match_group_read(
        &mut self,
        v: &hir::Expr,
        index: &ast::Expr,
        want: Want,
        span: Span,
    ) -> Option<hir::Expr> {
        let ast::ExprKind::Lit(ast::Lit::Int { .. }) = &index.kind else {
            return None;
        };
        if want == Want::BorrowMut || !self.is_regexp_match(v.ty) {
            return None;
        }
        let at = ast::Ident {
            name: "at".into(),
            span,
        };
        let recv = self.as_class_bound(v.clone());
        Some(self.method_call_on(recv, &at, &[], std::slice::from_ref(index), None, span))
    }
}
