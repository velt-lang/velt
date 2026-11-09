//! Index signatures (https://veltlang.com/docs/reference/types/ "Objects, arrays, tuples and maps"): an object type
//! or interface with only an index signature, `{ [key: string]: V }`, is `Record<string, V>`, as
//! the two are interchangeable in TypeScript. `readonly` is not checked yet, as for
//! `readonly T[]`. Named members next to an index signature (`{ name?: string; [key: string]:
//! string | undefined }`, `{ children?: JSXChildren; [key: string]: any }`) are keys of that
//! record too, so every key keeps JavaScript's order: the type is the record viewed with the
//! named members' types (a "record view"). Each named member's type must be assignable to `V`
//! (TypeScript's TS2411; an optional one with its `undefined`). A named member reads as its
//! declared type (an optional one with `undefined`), checked at run time because an index write
//! may store another `V`; next to `any` or `unknown` values only a required one does, and an
//! optional one reads as `V`. Any other key reads as `V`. A key of Velt's integer types is an
//! error saying it is not supported yet. A `number` key is a string key, as in JavaScript
//! (`[id: number]: V` is `Record<string, V>`; `r[1]` is `r["1"]`).

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
        let num = self.ty.f64;
        match self.ty.kind(k) {
            TyKind::Str | TyKind::Error => {}
            // `[key: number]`: JavaScript makes a number key its string, so this is the same
            // record (`Record<number, V>`, `crate::record_keys`).
            _ if k == num => {}
            TyKind::Int(_) | TyKind::Float(_) => {
                ok = false;
                self.error(
                    Diagnostic::error(
                        "an integer index signature is not supported yet (#937)",
                        key.span,
                    )
                    .with_note("use a `number` key (`{ [key: number]: V }`), an array or a `Map`"),
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
        // Named members: the record viewed with their types (module docs).
        let mut named = None;
        if let Some(f) = fields.first() {
            let tag = self.resolve_object(fields, env);
            if tag == self.ty.error
                || (v != self.ty.error && !self.named_members_fit(tag, fields, v))
            {
                ok = false;
            }
            named = Some((tag, f.name.span));
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
    pub(crate) fn record_view_type(
        &mut self,
        base: TyId,
        tag: TyId,
        module: usize,
        span: Span,
    ) -> TyId {
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

    /// The type named member `name` of record view `t` reads as: its declared type, with
    /// `undefined` for an optional one. `None` for another key, and for an optional member next
    /// to `any` or `unknown` values (it reads as the record's `V`, #930).
    pub(crate) fn record_view_member(&mut self, t: TyId, name: &str) -> Option<TyId> {
        let (base, tag) = self.record_view(t)?;
        let TyKind::Adt(d, _) = self.ty.kind(tag) else {
            return None;
        };
        let f = self.adt(*d)?.fields.iter().find(|f| f.name == name)?;
        let (ty, optional) = (f.ty, f.optional);
        if !optional {
            return Some(ty);
        }
        let TyKind::Adt(_, args) = self.ty.kind(base) else {
            return None;
        };
        let v = *args.get(1)?;
        if matches!(
            self.dynamic(v),
            Some(crate::any::Dynamic::Any | crate::any::Dynamic::Unknown)
        ) {
            return None;
        }
        let undefined = self.ty.undefined();
        Some(self.union_with(&[ty, undefined], None, Span::default()))
    }

    /// Is every named member of object type `tag` (written as `fields`) assignable to the index
    /// signature's values `v`? Reports each one that is not (TypeScript's TS2411).
    fn named_members_fit(&mut self, tag: TyId, fields: &[ast::ObjectTypeField], v: TyId) -> bool {
        if matches!(
            self.dynamic(v),
            Some(crate::any::Dynamic::Any | crate::any::Dynamic::Unknown)
        ) {
            return true;
        }
        let TyKind::Adt(d, _) = self.ty.kind(tag) else {
            return true;
        };
        let members: Vec<(String, TyId, bool)> = match self.adt(*d) {
            Some(a) => a
                .fields
                .iter()
                .map(|f| (f.name.clone(), f.ty, f.optional))
                .collect(),
            None => return true,
        };
        let mut ok = true;
        for (name, ty, optional) in members {
            let read = match optional {
                true => {
                    let undefined = self.ty.undefined();
                    self.union_with(&[ty, undefined], None, Span::default())
                }
                false => ty,
            };
            if self.ty.has_error(read) || self.form_assignable(read, v) {
                continue;
            }
            ok = false;
            let span = fields
                .iter()
                .find(|f| f.name.name == name)
                .map_or(fields[0].name.span, |f| f.name.span);
            let (shown, values) = (self.display(read), self.display(v));
            self.error(
                Diagnostic::error(
                    format!("the member `{name}` of type `{shown}` is not assignable to the index signature's type `{values}`"),
                    span,
                )
                .with_note("every named member is also a key of the index signature, so its type must be one of the values' types (an optional member's includes `undefined`)"),
            );
        }
        ok
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
