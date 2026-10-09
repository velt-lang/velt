//! An object type converts to another object type whose fields it has, by name: the same fields
//! in another order (`Aged & Named` where `Named & Aged` is expected, #651), or only some of them
//! (`A & B` where `A` is expected, #650). TypeScript is structural, so these are one object
//! there. Velt lays an object type out by its field order, so the conversion builds a new object
//! of the expected type from the source's field values: one allocation and a copy of each field
//! (a nested object or array is shared, not copied). A field the expected type marks optional
//! (`a?: T`) may be missing from the source (it is absent) or required there.
//!
//! Field types must be the same (up to `readonly`), or convert the same way (a nested object
//! type with the fields of the expected one). Fields that keep "absent" apart from `null`
//! (`a?: T | null`) are not converted. A copy that a later field assignment could tell from the
//! original is reported once every body is checked (`crate::object_copies`).

use crate::body::places::set_place_mode;
use crate::body::FnCx;
use crate::hir::{self, ExprKind as H, Intrinsic, TyId, TyKind, UseMode};
use crate::object_copies::Copy;

/// How deep nested object types may convert.
const MAX_DEPTH: u32 = 8;

/// Where one field of the converted object comes from.
enum Slot {
    /// Field `index` of the source, of type `from`, converted to `to`.
    Field { index: u32, from: TyId, to: TyId },
    /// An optional field the source doesn't have: absent.
    Absent(TyId),
}

impl FnCx<'_, '_> {
    /// `h` as a new object of object type `exp` (module docs), or `Err(h)` if it doesn't convert.
    pub(super) fn copy_object(&mut self, h: hir::Expr, exp: TyId) -> Result<hir::Expr, hir::Expr> {
        let Some(slots) = self.copy_plan(h.ty, exp, 0) else {
            return Err(h);
        };
        let path = is_field_path(&h);
        if self.detached && !path {
            return Err(h);
        }
        let TyKind::Adt(def, type_args) = self.cx.ty.kind(exp).clone() else {
            return Err(h);
        };
        let (span, from) = (h.span, h.ty);
        let fields = self.copied_names(exp, &slots);
        let source = match &h.kind {
            H::Call {
                callee: hir::Callee::Intrinsic(Intrinsic::Share),
                args,
            } => args.first().and_then(|a| self.place_text(a)),
            _ => self.place_text(&h),
        }
        .filter(|s| !s.starts_with('<'));
        let fresh = crate::fresh_returns::fresh_callees(&h).is_some_and(|c| c.is_empty());
        self.cx.object_copies.copies.push(Copy {
            from,
            to: exp,
            fields,
            span,
            source,
            fresh,
        });
        let mut lets = vec![];
        let mut base = match path {
            true => h,
            false => self.temp("<copy>", h, &mut lets),
        };
        set_place_mode(&mut base, UseMode::Borrow);
        let mut values = vec![];
        for slot in slots {
            let v = match slot {
                Slot::Field { index, from, to } => {
                    let v = self.copied_field(&base, index, from);
                    self.coerce(v, to)
                }
                Slot::Absent(ty) => self.mk(H::Lit(hir::Lit::Null), ty, span),
            };
            values.push(v);
        }
        let lit = H::AdtLit {
            def,
            type_args,
            fields: values,
        };
        let lit = self.mk(lit, exp, span);
        Ok(self.with_lets(lets, lit))
    }

    /// Does a `from` convert to a `to` by copying (module docs)?
    pub(super) fn copies_to(&mut self, from: TyId, to: TyId) -> bool {
        self.copy_plan(from, to, 0).is_some()
    }

    /// Field `index` of `base`: copied, or shared.
    fn copied_field(&mut self, base: &hir::Expr, index: u32, ty: TyId) -> hir::Expr {
        let copy = self.cx.is_copy(ty);
        let mode = if copy { UseMode::Copy } else { UseMode::Borrow };
        let kind = H::Field {
            base: Box::new(base.clone()),
            index,
            mode,
        };
        let v = self.mk(kind, ty, base.span);
        match copy {
            true => v,
            false => self.intrinsic(Intrinsic::Share, vec![v], ty, base.span),
        }
    }

    /// The names of the fields of `exp` that `slots` copy from the source.
    fn copied_names(&self, exp: TyId, slots: &[Slot]) -> Vec<String> {
        let TyKind::Adt(d, _) = self.cx.ty.kind(exp) else {
            return vec![];
        };
        let Some(a) = self.cx.adt(*d) else {
            return vec![];
        };
        a.fields
            .iter()
            .zip(slots)
            .filter(|(_, s)| matches!(s, Slot::Field { .. }))
            .map(|(f, _)| f.name.clone())
            .collect()
    }

    /// For each field of object type `to`, where it comes from in a `from` value; `None` when a
    /// `from` doesn't convert to a `to` this way (module docs).
    fn copy_plan(&mut self, from: TyId, to: TyId, depth: u32) -> Option<Vec<Slot>> {
        if depth > MAX_DEPTH
            || from == to
            || !self.cx.is_object_type(from)
            || !self.cx.is_object_type(to)
            || self.cx.canon(from) == self.cx.canon(to)
            || self.cx.same_layout(from, to)
        {
            return None;
        }
        let (TyKind::Adt(fd, fargs), TyKind::Adt(td, targs)) =
            (self.cx.ty.kind(from).clone(), self.cx.ty.kind(to).clone())
        else {
            return None;
        };
        let (fa, ta) = (self.cx.adt(fd)?, self.cx.adt(td)?);
        let (fkind, tkind) = (fa.kind, ta.kind);
        let (src, dst) = (fa.fields.clone(), ta.fields.clone());
        let mut slots = vec![];
        for g in &dst {
            let gty = self.cx.subst(g.ty, &targs);
            if crate::anon::has_presence(&self.cx.ty, tkind, g) {
                return None;
            }
            let Some(i) = src.iter().position(|f| f.name == g.name) else {
                if !g.optional {
                    return None;
                }
                slots.push(Slot::Absent(gty));
                continue;
            };
            let f = &src[i];
            if crate::anon::has_presence(&self.cx.ty, fkind, f) || (f.optional && !g.optional) {
                return None;
            }
            let fty = self.cx.subst(f.ty, &fargs);
            if !self.field_converts(fty, gty, g.optional && !f.optional, depth) {
                return None;
            }
            slots.push(Slot::Field {
                index: i as u32,
                from: fty,
                to: gty,
            });
        }
        Some(slots)
    }

    /// Does a field value of type `from` convert to `to` for a copy (the same type, up to
    /// `readonly`, or an object type that copies)? `wraps`: `to` is `from | null`, for an
    /// optional field the source has as a required one.
    fn field_converts(&mut self, from: TyId, to: TyId, wraps: bool, depth: u32) -> bool {
        let to = match (wraps, self.cx.ty.opt_payload(to)) {
            (true, Some(p)) => p,
            (true, None) => return false,
            (false, _) => to,
        };
        from == to
            || self.cx.canon(from) == self.cx.canon(to)
            || self.cx.same_layout(from, to)
            || self.copy_plan(from, to, depth + 1).is_some()
    }
}

/// Is `h` a local or a field path of one (`a`, `a.b.c`), which can be read again per field?
fn is_field_path(h: &hir::Expr) -> bool {
    match &h.kind {
        H::Local(..) => true,
        H::Field { base, .. } => is_field_path(base),
        _ => false,
    }
}
