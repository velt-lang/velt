//! Index signatures (docs/reference/types.md "Objects, arrays, tuples and maps"): an object type
//! or interface with only an index signature, `{ [key: string]: V }`, is `Record<string, V>`, as
//! the two are interchangeable in TypeScript. `readonly` is not checked yet, as for
//! `readonly T[]`. Named members next to an index signature whose values are `any` or `unknown`
//! (`{ children?: JSXChildren; [key: string]: any }`) are keys of that record too: every type is
//! one the values have, and a named member reads as `any` (or `unknown`) like any other key.
//! What Velt cannot represent exactly is an error saying it is not supported yet: named members
//! next to another index signature (a read of `name` would be `V | null`, and an optional one
//! needs `undefined`, #930) and a `number` key.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::ctx::Ctx;
use crate::hir::{TyId, TyKind};
use crate::resolve::TyEnv;

impl Ctx<'_> {
    /// `{ [key: K]: V }`: `Record<string, V>` (see the module docs).
    pub(crate) fn resolve_index_sig(
        &mut self,
        fields: &[ast::ObjectTypeField],
        key: &ast::TypeExpr,
        value: &ast::TypeExpr,
        env: &TyEnv,
    ) -> TyId {
        let k = self.resolve_type(key, env);
        let v = self.resolve_type(value, env);
        let mut ok = k != self.ty.error && v != self.ty.error;
        match self.ty.kind(k) {
            TyKind::Str | TyKind::Error => {}
            TyKind::Int(_) | TyKind::Float(_) => {
                ok = false;
                self.error(
                    Diagnostic::error(
                        "a `number` index signature is not supported yet (#937)",
                        key.span,
                    )
                    .with_note(
                        "use an array, a `Map<number, V>`, or a `string` key (`{ [key: string]: V }`)",
                    ),
                );
            }
            _ => {
                ok = false;
                let shown = self.display(k);
                self.error(
                    Diagnostic::error(
                        format!("an index signature's key must be `string` or `number`; found `{shown}`"),
                        key.span,
                    )
                    .with_note("for a fixed set of keys, write `Record<K, V>` or an object type"),
                );
            }
        }
        // Next to `[key: string]: any` (or `unknown`) every named member's type is one the
        // values have, so the type is that record, viewed with the named members' types.
        let dynamic_values = matches!(
            self.dynamic(v),
            Some(crate::any::Dynamic::Any | crate::any::Dynamic::Unknown)
        );
        let mut named = None;
        if dynamic_values && !fields.is_empty() {
            let tag = self.resolve_object(fields, env);
            if tag == self.ty.error {
                ok = false;
            }
            named = Some((tag, fields[0].name.span));
        } else if let Some(f) = fields.first() {
            ok = false;
            self.error(
                Diagnostic::error(
                    format!("a named member (`{}`) next to an index signature is not supported yet (#937)", f.name.name),
                    f.name.span,
                )
                .with_note("an object type with only an index signature is a `Record<string, V>`, and so is one with named members next to `[key: string]: any` (or `unknown`); next to other values, each would need its own type (and an optional one `undefined`, #930)"),
            );
        }
        if !ok {
            return self.ty.error;
        }
        let Some(record) = self.prelude_adt("Record") else {
            return self.ty.error;
        };
        let str_ = self.ty.str_;
        let base = self.ty.intern(TyKind::Adt(record, vec![str_, v]));
        match named {
            Some((tag, span)) => self.record_view_type(base, tag, env.module, span),
            None => base,
        }
    }

    /// The record `base` viewed with the named members of object type `tag` (module docs): a
    /// brand of the record (`crate::brands`), so it is that record for everything but reading
    /// a required named member, and lowering sees the record. A `tag` that mentions type
    /// parameters gives the record itself (its members read as `V`).
    fn record_view_type(&mut self, base: TyId, tag: TyId, module: usize, span: Span) -> TyId {
        let mut params = vec![];
        crate::types::collect_params(&self.ty, tag, &mut params);
        if !params.is_empty() {
            return base;
        }
        let view = self.brand_type(base, tag, module, span);
        if let TyKind::Adt(d, _) = self.ty.kind(view) {
            self.record_views.insert(*d, tag);
        }
        view
    }

    /// `(record, named members' object type)` if `t` is a record viewed with named members.
    pub(crate) fn record_view(&self, t: TyId) -> Option<(TyId, TyId)> {
        let TyKind::Adt(d, args) = self.ty.kind(t) else {
            return None;
        };
        if !args.is_empty() {
            return None;
        }
        let tag = *self.record_views.get(d)?;
        Some((*self.brands.get(d)?, tag))
    }

    /// The declared type of the required (not optional) named member `name` of record view
    /// `t`; `None` for an optional one or another key (they read as the record's `V`, #930).
    pub(crate) fn record_view_member(&self, t: TyId, name: &str) -> Option<TyId> {
        let (_, tag) = self.record_view(t)?;
        let TyKind::Adt(d, _) = self.ty.kind(tag) else {
            return None;
        };
        let f = self.adt(*d)?.fields.iter().find(|f| f.name == name)?;
        (!f.optional).then_some(f.ty)
    }
}

impl crate::body::FnCx<'_, '_> {
    /// `h` as its record when its type is a record view (the same value).
    pub(crate) fn unview(&mut self, mut h: crate::hir::Expr) -> crate::hir::Expr {
        if let Some((base, _)) = self.cx.record_view(h.ty) {
            h.ty = base;
        }
        h
    }
}
