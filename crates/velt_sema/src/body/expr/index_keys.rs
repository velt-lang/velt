//! `m[i, j]`: an index with several keys (design operators.md step 8, #2179). It calls the
//! object's operator indexer with every key, `m[Symbol.index](i, j)` and, as a target,
//! `m[Symbol.setIndex](i, j, v)` (`indexers.rs`, which evaluates the object, then each key
//! once, left to right). A key naming a member never applies: several keys always call the
//! indexer. Every other type is an error with the fix `a[i][j]`, since JavaScript's comma
//! operator would read `a[j]` there.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;
use velt_syntax::operators::INDEX;

use super::assign::AssignTarget;
use super::indexers::keys_of;
use crate::body::{FnCx, Want};
use crate::hir::{self, TyId, TyKind};

impl FnCx<'_, '_> {
    /// `object[index, more…]` (`object?.[…]` with `optional`) read.
    pub(super) fn multi_index_expr(
        &mut self,
        object: &ast::Expr,
        index: &ast::Expr,
        more: &[ast::Expr],
        optional: bool,
        span: Span,
    ) -> hir::Expr {
        let keys = keys_of(index, more);
        if optional {
            // The keys are evaluated only when `object` is neither `null` nor `undefined`.
            return self.optional_chain(object, span, |s, v| s.keys_read(v, &keys, span));
        }
        let obj = self.receiver_expr(object);
        self.keys_read(obj, &keys, span)
    }

    /// The checked object `obj` read at `keys`.
    fn keys_read(&mut self, obj: hir::Expr, keys: &[&ast::Expr], span: Span) -> hir::Expr {
        let shared = match *self.cx.ty.kind(obj.ty) {
            TyKind::Shared(t) => Some(t),
            _ => None,
        };
        let obj = match shared {
            Some(t) if self.has_indexer(t) => self.frozen_view(obj, None),
            _ => obj,
        };
        if !self.keys_fit(obj.ty, keys, span) {
            return self.error_expr(span);
        }
        self.indexer_read_keys(obj, keys, span)
    }

    /// `object[index, more…]` as an assignment target: the checked object, written through
    /// `[Symbol.setIndex]` (`indexer_assign`).
    pub(super) fn multi_index_target(
        &mut self,
        object: &ast::Expr,
        index: &ast::Expr,
        more: &[ast::Expr],
        span: Span,
    ) -> Option<AssignTarget> {
        let keys = keys_of(index, more);
        let obj = self.assign_receiver(object);
        let obj = self.unbrand_instance(obj);
        if let TyKind::Shared(t) = *self.cx.ty.kind(obj.ty) {
            if self.has_indexer(t) {
                self.frozen_write(&obj, span);
                self.check_keys(&keys);
                return None;
            }
        }
        if !self.keys_fit(obj.ty, &keys, span) {
            return None;
        }
        Some(AssignTarget::Indexer(obj))
    }

    /// Does type `t` take `keys` through its indexer? Reported when it has none, or one that
    /// takes a single key: `a[i][j]` is what was meant.
    fn keys_fit(&mut self, t: TyId, keys: &[&ast::Expr], span: Span) -> bool {
        if self.cx.ty.is_bottom(t) {
            self.check_keys(keys);
            return false;
        }
        let tn = self.cx.display(t);
        let fix = "to index an element of an element, write `a[i][j]`";
        let comma =
            "in TypeScript `a[x, j]` is the comma operator: it evaluates `x` and reads `a[j]`; \
                     Velt has no comma operator, so evaluate `x` first and write `a[j]`";
        if !self.has_indexer(t) {
            self.check_keys(keys);
            self.cx.error(
                Diagnostic::error(
                    format!("`{tn}` has no indexer taking several keys: an index with several keys calls `[Symbol.index]`"),
                    span,
                )
                .with_note(fix)
                .with_note(comma),
            );
            return false;
        }
        if self.indexer_arity(t) == Some(1) {
            self.check_keys(keys);
            self.cx.error(
                Diagnostic::error(
                    format!(
                        "`{tn}`'s `[Symbol.index]` takes one key, not {}",
                        keys.len()
                    ),
                    span,
                )
                .with_note(fix)
                .with_note(comma),
            );
            return false;
        }
        true
    }

    /// The number of keys `t`'s `[Symbol.index]` takes, when it is known.
    fn indexer_arity(&mut self, t: TyId) -> Option<usize> {
        use super::method::Resolved;
        match self.resolve_method(t, INDEX)? {
            Resolved::Def { def, .. } | Resolved::Virtual { def, .. } => {
                Some(self.cx.fn_info(def).params.len())
            }
            Resolved::Iface { method, .. } => Some(method.params.len()),
            Resolved::Builtin(_) => None,
        }
    }

    /// Check `keys` for their own errors after the index itself was rejected.
    fn check_keys(&mut self, keys: &[&ast::Expr]) {
        for k in keys {
            self.expr(k, None, Want::Borrow);
        }
    }
}
