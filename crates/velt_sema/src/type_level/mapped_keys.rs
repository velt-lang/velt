//! Mapped types over keys that are not literals (`{ [K in keyof R]: R[K] }` over a
//! `Record<string, V>`): TypeScript's index signatures, records here.

use velt_syntax::ast;

use crate::ctx::Ctx;
use crate::hir::{Nothing, TyId, TyKind};
use crate::resolve::TyEnv;

impl Ctx<'_> {
    /// A mapped type over keys that are `string`, `number` and `symbol` (not literals), at
    /// least one `string`, without an `as` clause (`{ readonly [K in keyof R]: R[K] }` over a
    /// `Record<string, V>`, whose keys are `string | number`): TypeScript's index signature,
    /// a `Record<string, V>` with `V` the value at `K = string` (number keys are strings in
    /// JavaScript; symbol keys of a record are not listed). `None` for other keys.
    pub(super) fn mapped_index_signature(
        &mut self,
        m: &ast::MappedType,
        env: &TyEnv,
        src: TyId,
    ) -> Option<TyId> {
        if m.name.is_some() {
            return None;
        }
        let members = self.union_members(src).unwrap_or_else(|| vec![src]);
        let str_ = self.ty.str_;
        let plain = members.iter().all(|&k| {
            k == str_ || self.ty.is_numeric(k) || matches!(self.ty.kind(k), TyKind::Symbol)
        });
        if !plain || !members.contains(&str_) {
            return None;
        }
        let mut inner = env.clone();
        inner.locals.push((m.param.name.clone(), str_));
        let v = self.resolve_type(&m.value, &inner);
        if v == self.ty.error {
            return Some(v);
        }
        let v = match m.optional {
            ast::MappedModifier::Add => self.ty.option_of(v, Nothing::Undefined),
            _ => v,
        };
        let record = self.prelude_adt("Record")?;
        Some(self.ty.intern(TyKind::Adt(record, vec![str_, v])))
    }
}
