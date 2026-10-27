//! `r[k]` of a string-keyed record with a `string | symbol` key (sigx's
//! `(obj as Record<string, any>)[key]` with `key` a proxy trap's property, which TypeScript
//! accepts where narrowing leaves `key` a `string`): in JavaScript a symbol is never one of the
//! record's string keys. The read is the
//! prelude's `__recordGetKeyed(r, k)`: the entry for a string, `undefined` for a symbol. Only a
//! key written as a name is looked at, so other reads cost nothing more.

use velt_common::Span;
use velt_syntax::ast;

use crate::body::FnCx;
use crate::hir;

impl FnCx<'_, '_> {
    /// `obj[index]` of a `Record<string, V>` with an `index` of type `string | symbol`.
    /// `Err(obj)` for any other read.
    pub(super) fn record_symbol_key_read(
        &mut self,
        obj: hir::Expr,
        index: &ast::Expr,
        span: Span,
    ) -> Result<hir::Expr, hir::Expr> {
        let Some((k, _)) = self.record_args(obj.ty) else {
            return Err(obj);
        };
        // A name as the key (sigx's), typed on trial: other keys keep the check of `record_read`.
        if k != self.cx.ty.str_ || !matches!(index.kind, ast::ExprKind::Ident(_)) {
            return Err(obj);
        }
        let symbolic = self
            .trial_type(index)
            .is_some_and(|t| self.string_or_symbol(t));
        if !symbolic {
            return Err(obj);
        }
        let at = obj.span;
        Ok(self.with_temp("<record>", obj, at, span, |cx, r| {
            let args = vec![r.clone(), index.clone()];
            cx.prelude_fn_call("__recordGetKeyed", args, span)
        }))
    }
}
