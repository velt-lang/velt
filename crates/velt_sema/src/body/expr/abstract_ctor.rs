//! TypeScript's TS2715: an abstract property (or abstract accessor) of a class read or written
//! through `this` in that class's own constructor, where no subclass field initializer has run.

use velt_common::Span;

use super::method::Resolved;
use crate::body::{FnCx, FnKind, LocalKind};
use crate::defs::member_key;
use crate::hir::{self, ExprKind as H};

impl FnCx<'_, '_> {
    /// Report `this.name` (`setter`: `this.name = …`) in the constructor of the class declaring
    /// `name` abstract.
    pub(super) fn abstract_in_ctor(
        &mut self,
        obj: &hir::Expr,
        name: &str,
        setter: bool,
        span: Span,
    ) {
        let is_this = matches!(obj.kind, H::Local(l, _) if self.local_kind(l) == LocalKind::This);
        if self.f.kind != FnKind::Ctor || !is_this {
            return;
        }
        let def = match self.resolve_method(obj.ty, &member_key(name, setter)) {
            Some(Resolved::Def { def, .. } | Resolved::Virtual { def, .. }) => def,
            _ => return,
        };
        let owner = self.cx.fn_info(def).owner;
        if !self.cx.abstract_methods.contains(&def) || owner != self.owner {
            return;
        }
        let class = owner
            .and_then(|o| self.cx.adt(o))
            .map(|a| a.name.clone())
            .unwrap_or_default();
        self.cx.err(
            format!("abstract property `{name}` of `{class}` cannot be used in its constructor (TypeScript's TS2715)"),
            span,
        );
    }
}
