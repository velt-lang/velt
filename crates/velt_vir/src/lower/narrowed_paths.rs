//! The source form of a narrowed field path that lowering checks again (`place.rs`
//! `check_narrowed_field`), for its `TypeError` (`state.next`, `this.left.right`).

use velt_sema::hir::{self, ExprKind as E, TyKind};

use super::FnLower;

impl FnLower<'_, '_> {
    /// `state.next` for the place `inner` (a local or module variable and its fields, under
    /// payload projections); `None` for anything else (a record's `r[k]`, a temporary).
    pub(super) fn narrowed_path_name(&mut self, inner: &hir::Expr) -> Option<String> {
        match &inner.kind {
            E::Local(l, _) => {
                let v = self.info.get(l.0 as usize)?.vir?;
                self.locals.get(v.0 as usize)?.name.clone()
            }
            E::Global(d) => match self.cx.hir.def(*d) {
                hir::Def::Global(g) => g.name.rsplit("::").next().map(str::to_string),
                _ => None,
            },
            E::Field { base, index, .. } => {
                let root = self.narrowed_path_name(base)?;
                let bty = self.sub(base.ty);
                let TyKind::Adt(d, _) = self.cx.kind(bty) else {
                    return None;
                };
                let hir::Def::Adt(a) = self.cx.hir.def(d) else {
                    return None;
                };
                let field = &a.fields.get(*index as usize)?.name;
                Some(format!("{root}.{field}"))
            }
            E::UnwrapSome(base, _) | E::UnwrapVariant { expr: base, .. } | E::Downcast(base) => {
                self.narrowed_path_name(base)
            }
            _ => None,
        }
    }

    /// The `TypeError` of a narrowed read of `inner` that found `what` (`null`, `a value of
    /// another type`) instead.
    pub(super) fn narrowed_read_error(&mut self, inner: &hir::Expr, what: &str) -> String {
        let suffix = self.panic_suffix();
        match self.narrowed_path_name(inner) {
            Some(path) => format!(
                "TypeError: `{path}` was checked, but it was set to {what} before this read{suffix}"
            ),
            None => {
                format!("TypeError: a narrowed field was set to {what} before this read{suffix}")
            }
        }
    }
}
