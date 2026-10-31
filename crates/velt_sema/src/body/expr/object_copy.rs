//! An object type converts to another object type whose fields it has, by name: the same fields
//! in another order (`Aged & Named` where `Named & Aged` is expected, #651), or only some of them
//! (`A & B` where `A` is expected, #650). TypeScript is structural, so these are one object
//! there. Velt lays an object type out by its field order, so the conversion builds a new object
//! of the expected type from the source's field values: one allocation and a copy of each field
//! (a nested object or array is shared, not copied). A field the expected type marks optional
//! (`a?: T`) may be missing from the source (it is absent) or required there.
//!
//! Field types must be the same (up to `readonly`), or convert the same way (a nested object
//! type with the fields of the expected one). An optional source field (which keeps "absent"
//! apart from a present `undefined`) is not converted; an optional target field the source
//! lacks is absent, and one the source has as a required field is present. A copy that a later field assignment could tell from the
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
    /// Optional field `index` of the source (of type `ty`, kept apart from absent by its presence
    /// flag) into an optional field of the same type: absent in the literal, then set when the
    /// source has it.
    Optional { index: u32, ty: TyId },
    /// An optional field the source doesn't have: absent (with its presence flag clear when
    /// the field keeps one).
    Absent(TyId, bool),
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
        let (fields, absent) = self.copied_names(exp, &slots);
        let source = match &h.kind {
            H::Call {
                callee: hir::Callee::Intrinsic(Intrinsic::Share),
                args,
            } => args.first().and_then(|a| self.place_text(a)),
            _ => self.place_text(&h),
        }
        .filter(|s| !s.starts_with('<'));
        let fresh = crate::fresh_returns::fresh_callees(&h).is_some_and(|c| c.is_empty());
        let in_order = self.in_order(from, &fields, &slots);
        self.cx.object_copies.copies.push(Copy {
            from,
            to: exp,
            fields,
            absent,
            span,
            source,
            fresh,
            in_order,
            generic: false,
        });
        let mut lets = vec![];
        let mut base = match path {
            true => h,
            false => self.temp("<copy>", h, &mut lets),
        };
        set_place_mode(&mut base, UseMode::Borrow);
        let mut values = vec![];
        let mut posts = vec![];
        for (k, slot) in slots.into_iter().enumerate() {
            let v = match slot {
                Slot::Field { index, from, to } => {
                    let v = self.copied_field(&base, index, from);
                    self.coerce(v, to)
                }
                Slot::Optional { index, ty } => {
                    posts.push((k as u32, index, ty));
                    self.intrinsic(Intrinsic::FieldAbsent, vec![], ty, span)
                }
                Slot::Absent(ty, true) => self.intrinsic(Intrinsic::FieldAbsent, vec![], ty, span),
                // `b?: any` left out: the dynamic `undefined`, as in an object literal.
                Slot::Absent(ty, false) if self.cx.dynamic(ty).is_some() => {
                    self.dynamic_nothing(hir::Nothing::Undefined, ty, span)
                }
                Slot::Absent(ty, false) => self.mk(H::Lit(hir::Lit::Null), ty, span),
            };
            values.push(v);
        }
        let lit = H::AdtLit {
            def,
            type_args,
            fields: values,
            converted: hir::Conversion::Copy,
        };
        let lit = self.mk(lit, exp, span);
        if posts.is_empty() {
            return Ok(self.with_lets(lets, lit));
        }
        let tmp = self.new_local("<copy>", exp, true, span, crate::body::LocalKind::Temp);
        lets.push(hir::Stmt {
            kind: hir::StmtKind::Let {
                local: tmp,
                init: Some(lit),
            },
            span,
        });
        for (k, index, ty) in posts {
            let src = self.mk(
                H::Field {
                    base: Box::new(base.clone()),
                    index,
                    mode: UseMode::Borrow,
                },
                ty,
                span,
            );
            let b = self.cx.ty.bool_;
            let cond = self.intrinsic(Intrinsic::FieldPresent, vec![src], b, span);
            let value = self.copied_field(&base, index, ty);
            let target = self.mk(H::Local(tmp, UseMode::Borrow), exp, span);
            let place = H::Field {
                base: Box::new(target),
                index: k,
                mode: UseMode::Borrow,
            };
            let place = self.mk(place, ty, span);
            let unit = self.cx.ty.unit;
            let assign = H::Assign {
                place: Box::new(place),
                value: Box::new(value),
            };
            let assign = self.mk(assign, unit, span);
            let then = hir::Block {
                stmts: vec![hir::Stmt {
                    kind: hir::StmtKind::Expr(assign),
                    span,
                }],
                value: None,
                span,
            };
            lets.push(hir::Stmt {
                kind: hir::StmtKind::If {
                    cond,
                    then,
                    els: None,
                },
                span,
            });
        }
        let result = self.mk(H::Local(tmp, UseMode::Move), exp, span);
        Ok(self.with_lets(lets, result))
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

    /// Does the copy have the source's fields (`copied`, in the expected type's order), in the
    /// source's order, each of the same type (no nested copy)?
    fn in_order(&mut self, from: TyId, copied: &[String], slots: &[Slot]) -> bool {
        let TyKind::Adt(d, _) = self.cx.ty.kind(from) else {
            return false;
        };
        let Some(a) = self.cx.adt(*d) else {
            return false;
        };
        a.fields.len() == copied.len()
            && a.fields.iter().zip(copied).all(|(f, c)| f.name == *c)
            && (slots.iter()).all(|s| match s {
                // A value boxed as an `any` shows as itself (`boxes_shared`).
                Slot::Field { from, to, .. } => {
                    from == to
                        || (self.cx.dynamic(*to).is_some() && boxes_shared(self.cx, *from, 0))
                }
                _ => true,
            })
    }

    /// The names of the fields of `exp` that `slots` copy from the source, and of the optional
    /// ones it leaves absent.
    fn copied_names(&self, exp: TyId, slots: &[Slot]) -> (Vec<String>, Vec<String>) {
        let (mut copied, mut absent) = (vec![], vec![]);
        let TyKind::Adt(d, _) = self.cx.ty.kind(exp) else {
            return (copied, absent);
        };
        let Some(a) = self.cx.adt(*d) else {
            return (copied, absent);
        };
        for (f, s) in a.fields.iter().zip(slots) {
            match s {
                Slot::Field { .. } | Slot::Optional { .. } => copied.push(f.name.clone()),
                Slot::Absent(..) => absent.push(f.name.clone()),
            }
        }
        (copied, absent)
    }

    /// Is `t` the class of `import.meta` (std's `ImportMeta`)? It converts to an object type
    /// with some of its fields as an object type does (`f(import.meta)` for `p: { url: string
    /// }`), since `import.meta` is an object in TypeScript.
    pub(super) fn is_import_meta_class(&self, t: TyId) -> bool {
        let TyKind::Adt(d, _) = self.cx.ty.kind(t) else {
            return false;
        };
        self.cx.adt(*d).is_some_and(|a| {
            a.kind == hir::AdtKind::Class
                && self.cx.modules[a.module].path == velt_common::std_module!(lang_import_meta)
        })
    }

    /// For each field of object type `to`, where it comes from in a `from` value; `None` when a
    /// `from` doesn't convert to a `to` this way (module docs).
    fn copy_plan(&mut self, from: TyId, to: TyId, depth: u32) -> Option<Vec<Slot>> {
        if depth > MAX_DEPTH
            || from == to
            || !(self.cx.is_object_type(from) || self.is_import_meta_class(from))
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
            let Some(i) = src.iter().position(|f| f.name == g.name) else {
                if !g.optional {
                    return None;
                }
                let flag = crate::anon::has_presence(&self.cx.ty, tkind, g);
                slots.push(Slot::Absent(gty, flag));
                continue;
            };
            let f = &src[i];
            if f.optional && !g.optional {
                return None;
            }
            let fty = self.cx.subst(f.ty, &fargs);
            if crate::anon::has_presence(&self.cx.ty, fkind, f) {
                // Kept as is only into an optional field of the same type that keeps a flag too.
                let flag = crate::anon::has_presence(&self.cx.ty, tkind, g);
                let same = fty == gty || self.cx.canon(fty) == self.cx.canon(gty);
                if !flag || !same {
                    return None;
                }
                slots.push(Slot::Optional {
                    index: i as u32,
                    ty: gty,
                });
                continue;
            }
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
            // Equal but for their error-set variables (`throws::vars`), which become one set.
            || (self.cx.fn_sites.use_vars && self.cx.unify_err_vars(from, to))
            // A function that throws into a function type written without `throws` (its sites
            // take its errors), converted through a wrapper as a value is.
            || self.cx.fn_throws_differ(from, to) == Some(true)
            // A value boxed into a field of a dynamic type (`{ value: [any, any] }` as
            // `{ value: any }`, an `IteratorResult<any>`; #1185).
            || (self.cx.dynamic(from).is_none()
                && matches!(
                    self.cx.dynamic(to),
                    Some(crate::any::Dynamic::Any | crate::any::Dynamic::Unknown)
                )
                && !matches!(self.cx.ty.kind(from), TyKind::Promise(..) | TyKind::Unit | TyKind::Never)
                // In an interface adapter, a method's result converted as the call returns it
                // (an iterator's `IteratorResult<[any, any]>` as `IteratorResult<any>`).
                && (self.adapting || boxes_shared(self.cx, from, 0)))
            || self.copy_plan(from, to, depth + 1).is_some()
    }
}

/// Is a value of type `t` boxed as an `any` the same value, or one the program cannot tell from
/// it: a primitive, or an object held by reference (an array, map, set, class instance,
/// function or dynamic value)? A tuple or an object type may be stored inline and boxed as a
/// copy, which a later write into the original's element or field would not reach.
fn boxes_shared(cx: &mut crate::ctx::Ctx, t: TyId, depth: u32) -> bool {
    if depth > 8 {
        return false;
    }
    if let Some(p) = cx.ty.opt_payload(t) {
        return boxes_shared(cx, p, depth + 1);
    }
    match cx.ty.kind(t) {
        TyKind::Tuple(_) => false,
        TyKind::Adt(d, _) => match cx.adt(*d) {
            Some(a) => a.kind == crate::hir::AdtKind::Class,
            // A union (an enum of members): each member.
            None => match cx.union_members(t) {
                Some(ms) => ms.iter().all(|m| boxes_shared(cx, *m, depth + 1)),
                None => true,
            },
        },
        _ => true,
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
