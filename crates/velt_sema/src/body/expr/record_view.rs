//! Reads of a record viewed with named members (`crate::index_signatures`): a required named
//! member next to `[key: string]: any` (or `unknown`) reads as its declared type.

use velt_common::Span;
use velt_syntax::ast;

use super::record_call::RecordKey;
use crate::body::FnCx;
use crate::hir;

impl FnCx<'_, '_> {
    /// `obj.name` of a required named member of a record view: its declared type, checked at
    /// run time (an index write may have stored another type, as TypeScript allows); `Err`
    /// gives `obj` back for any other read.
    pub(super) fn record_view_read(
        &mut self,
        obj: hir::Expr,
        prop: &ast::Ident,
        span: Span,
    ) -> Result<hir::Expr, hir::Expr> {
        let Some(member) = self.cx.record_view_member(obj.ty, &prop.name) else {
            return Err(obj);
        };
        let obj = self.unview(obj);
        let read = self.record_read(obj, RecordKey::Name(prop), span);
        let read = self.collapse_dynamic_option(read);
        Ok(self.cast_dynamic(read, member))
    }
}
