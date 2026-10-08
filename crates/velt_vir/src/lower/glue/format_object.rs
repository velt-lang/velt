//! Objects as `console.log` prints them, like Node: `Name { a: 1, b: 'x' }` (`{ … }` for an
//! anonymous object type), `Name {}` without shown fields, `[Name]` (`[Object]`) past the depth
//! limit. Private zero-sized fields are hidden.

use velt_sema::hir::{AdtKind, TyId, TyKind};

use crate::lower::FnLower;
use crate::vir::{Operand, Place};

impl FnLower<'_, '_> {
    /// An object `Name { a: 1, b: 'x' }` (`{ … }` without a name), `Name {}` when it shows no
    /// fields, else `[Name]` (`[Object]`) past node's depth limit; `place` holds the struct
    /// value or, for classes, the object pointer. `obj`: the object's address when it may be
    /// part of a cycle.
    pub(super) fn format_object(
        &mut self,
        buf: &Operand,
        name: Option<String>,
        place: &Place,
        ty: TyId,
        obj: Option<Operand>,
        depth: &Operand,
    ) {
        let shown = self.shown_fields(ty);
        if shown.is_empty() {
            let text = match &name {
                Some(n) => format!("{n} {{}}"),
                None => "{}".into(),
            };
            return self.push_text(buf, &text);
        }
        let cut = format!("[{}]", name.as_deref().unwrap_or("Object"));
        self.within_depth(buf, depth, &cut, obj.clone(), |lw, child| match obj {
            Some(p) => lw.format_once(buf, p, |lw| {
                lw.format_fields(buf, name, place, ty, &shown, &child)
            }),
            None => lw.format_fields(buf, name, place, ty, &shown, &child),
        });
    }

    /// The fields an object prints: (index, name, type). ES private fields (`#x`) are hidden, as
    /// Node hides them, and so are private zero-sized fields (a marker such as std's
    /// `runtime: RuntimeHandle`); other private fields show, as Node shows a TypeScript
    /// `private` field.
    fn shown_fields(&mut self, ty: TyId) -> Vec<(u32, String, TyId)> {
        let TyKind::Adt(d, _) = self.cx.kind(ty) else {
            crate::lower::ice("field format of a non-struct type")
        };
        let all: Vec<(u32, String, TyId, bool)> = self
            .cx
            .adt_def(d)
            .fields
            .iter()
            .map(|f| (f.name.clone(), f.private))
            .zip(self.cx.adt_field_tys(ty))
            .enumerate()
            .map(|(i, ((n, private), t))| (i as u32, n, t, private))
            .collect();
        all.into_iter()
            .filter(|(_, n, t, private)| {
                !n.starts_with('#') && !(*private && self.is_empty_struct(*t))
            })
            .map(|(i, n, t, _)| (i, n, t))
            .collect()
    }

    /// `Name { a: 1, b: 'x' }` (or `{ … }` without a name) for the (non-empty) `shown` fields,
    /// each at depth `child`.
    fn format_fields(
        &mut self,
        buf: &Operand,
        name: Option<String>,
        place: &Place,
        ty: TyId,
        shown: &[(u32, String, TyId)],
        child: &Operand,
    ) {
        // The opening text and the first field's name form one static chunk.
        let mut pending = match &name {
            Some(n) => format!("{n} {{ "),
            None => "{ ".into(),
        };
        for (i, (index, n, t)) in shown.iter().enumerate() {
            let sep = if i > 0 { ", " } else { "" };
            pending.push_str(&format!("{sep}{n}: "));
            self.push_text(buf, &pending);
            pending.clear();
            let fp = self.field_place(place, ty, *index);
            self.format_nested(buf, &fp, *t, child);
        }
        self.push_text(buf, " }");
    }

    /// A struct without fields (zero-sized).
    fn is_empty_struct(&self, t: TyId) -> bool {
        matches!(self.cx.kind(t), TyKind::Adt(d, _)
            if matches!(self.cx.hir.def(d), velt_sema::hir::Def::Adt(a)
                if a.kind == AdtKind::Struct && a.fields.is_empty()))
    }
}
