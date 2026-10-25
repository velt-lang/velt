//! Object types intersected with a record of `any` or `unknown` values
//! (`{ server?: boolean } & Record<string, unknown>`): the record viewed with the object types'
//! members, as an index signature next to named members is (`crate::index_signatures`).

use velt_common::Span;

use crate::ctx::Ctx;
use crate::hir::{TyId, TyKind};

impl Ctx<'_> {
    /// `{ server?: boolean } & Record<string, unknown>`: object types and one record of `any`
    /// or `unknown` values are the record viewed with the object types' members, as
    /// `{ server?: boolean; [k: string]: unknown }` is (`crate::index_signatures`). `None` for
    /// other operands.
    pub(super) fn record_intersection(
        &mut self,
        operands: &[(TyId, Span)],
        module: usize,
        span: Span,
    ) -> Option<TyId> {
        let record = self.prelude_adt("Record")?;
        let is_record = |cx: &Self, t: TyId| {
            matches!(cx.ty.kind(t), TyKind::Adt(d, a) if *d == record
            && a.first() == Some(&cx.ty.str_)
            && a.get(1).is_some_and(|v| matches!(
                cx.dynamic(*v),
                Some(crate::any::Dynamic::Any | crate::any::Dynamic::Unknown)
            )))
        };
        let records: Vec<TyId> = operands
            .iter()
            .map(|&(o, _)| o)
            .filter(|&o| is_record(self, o))
            .collect();
        let [base] = records[..] else {
            return None;
        };
        let objects: Vec<TyId> = operands
            .iter()
            .map(|&(o, _)| o)
            .filter(|&o| o != base)
            .collect();
        if objects.is_empty() || !objects.iter().all(|&o| self.is_object_type(o)) {
            return None;
        }
        let mut tag = objects[0];
        for &o in &objects[1..] {
            tag = self.meet(tag, o, module, 1).ok()?;
        }
        Some(self.record_view_type(base, tag, module, span))
    }
}
