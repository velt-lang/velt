//! `(x as Record<K, V>)[k]` and `(x as Record<K, V>).name` on an `any` `x`, read or written.
//! TypeScript's assertion converts nothing: the key is read from or written to `x` itself. Velt
//! reads and writes it through `x` as an `any` (`dynamic_write`, `dynamic_member`), so a write
//! reaches the object `x` holds, as in Node, where converting `x` to the record first would
//! check its type (a `Record<string, any>` is not a `Record<string, V>`) and write into a copy.
//! The value written is checked as a `V`; a read is a `V | undefined`, as a record's key is.
//! The standard `{} as D` of a deferred `D` (generic_member.rs) is such an `any`: sigx fills
//! `{} as ToSignals<T>` through `(result as Record<string, PropertySignal<unknown>>)[key] = v`.

use velt_common::Span;
use velt_syntax::ast;

use super::object_builtins::strip_parens;
use crate::any::Dynamic;
use crate::body::{FnCx, Want};
use crate::hir::{self, Intrinsic, TyId};

impl FnCx<'_, '_> {
    /// `x` of the receiver `x as Record<K, V>` on an `any` `x`, and `V`; `None` for any other
    /// receiver (nothing is checked then).
    pub(super) fn any_as_record<'e>(
        &mut self,
        object: &'e ast::Expr,
    ) -> Option<(&'e ast::Expr, TyId)> {
        let ast::ExprKind::Cast { expr, ty } = &strip_parens(object).kind else {
            return None;
        };
        // The assertion's type is resolved again where it is checked as one: only a record is
        // kept here, and its diagnostics are reported there.
        let diags = self.cx.diags.len();
        let target = self.resolve(ty);
        if self.cx.diags.len() > diags {
            self.cx.diags.truncate(diags);
            return None;
        }
        let (_, v) = self.record_args(target)?;
        let t = self.trial_type(expr)?;
        (self.cx.dynamic(t) == Some(Dynamic::Any)).then_some((&**expr, v))
    }

    /// The assignment target `(x as Record<K, V>)[k]` / `.name` on an `any` `x`: `x` as the
    /// dynamic receiver, a value written checked as `V`; `None` for another receiver.
    pub(super) fn any_as_record_target(
        &mut self,
        object: &ast::Expr,
    ) -> Option<Option<super::assign::AssignTarget>> {
        let (x, v) = self.any_as_record(object)?;
        let obj = self.expr(x, None, Want::Borrow);
        match self.dynamic_write_receiver(obj) {
            Ok(d) => Some(d.map(|d| super::assign::AssignTarget::Dynamic(d, Some(v)))),
            Err(_) => None,
        }
    }

    /// `(x as Record<K, V>)[k]` / `.name` read on an `any` `x` (module docs); `None` for another
    /// receiver.
    pub(super) fn any_as_record_read(
        &mut self,
        object: &ast::Expr,
        key: AnyRecordKey<'_>,
        optional: bool,
        want: Want,
        span: Span,
    ) -> Option<hir::Expr> {
        if want == Want::BorrowMut || optional {
            return None;
        }
        let (x, v) = self.any_as_record(object)?;
        let obj = self.expr(x, None, Want::Borrow);
        let read = match key {
            AnyRecordKey::Index(index) => self.dynamic_index(obj, index, want, span),
            AnyRecordKey::Name(prop) => self.dynamic_get(obj, &prop.name, want, span),
        };
        let read = match read {
            Ok(h) | Err(h) => h,
        };
        if self.cx.ty.is_bottom(read.ty) || self.cx.dynamic(read.ty).is_none() {
            return Some(read);
        }
        let ty = match self.cx.dynamic(v) {
            Some(_) => v,
            None => self.cx.ty.option_of(v, hir::Nothing::Undefined),
        };
        Some(self.intrinsic(Intrinsic::AnyTo, vec![read], ty, span))
    }
}

/// The key of [`FnCx::any_as_record_read`].
pub(super) enum AnyRecordKey<'a> {
    Index(&'a ast::Expr),
    Name(&'a ast::Ident),
}
