//! Objects as `console.log` prints them, like Node: `Name { a: 1, b: 'x' }` (`{ … }` for an
//! anonymous object type), `Name {}` without shown fields, `[Name]` (`[Object]`) past the depth
//! limit. Private zero-sized fields are hidden, and so is an absent optional field (`a?: T`) of an
//! object type, as Node leaves out a missing key.

use velt_sema::hir::{AdtKind, TyId, TyKind};

use crate::lower::FnLower;
use crate::vir::{self, Operand, Place, Proj, Rvalue, Ty};

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
        let optional = self.optional_fields(ty, &shown);
        let cut = format!("[{}]", name.as_deref().unwrap_or("Object"));
        self.within_depth(buf, depth, &cut, obj.clone(), |lw, child| {
            let body = |lw: &mut Self| match optional.iter().any(|o| *o) {
                true => lw.format_fields_optional(buf, name, place, ty, &shown, &optional, &child),
                false => lw.format_fields(buf, name, place, ty, &shown, &child),
            };
            match obj {
                Some(p) => lw.format_once(buf, p, body),
                None => body(lw),
            }
        });
    }

    /// For each of the `shown` fields: is it optional (`a?: T`), so that it shows only while
    /// present? Not in a class: Node shows a class's optional field, an own key from the start.
    fn optional_fields(&mut self, ty: TyId, shown: &[(u32, String, TyId)]) -> Vec<bool> {
        let TyKind::Adt(d, _) = self.cx.kind(ty) else {
            return vec![false; shown.len()];
        };
        let def = self.cx.adt_def(d);
        if def.kind == AdtKind::Class {
            return vec![false; shown.len()];
        }
        let flags: Vec<bool> = shown
            .iter()
            .map(|(i, _, _)| def.fields[*i as usize].optional)
            .collect();
        shown
            .iter()
            .zip(flags)
            .map(|((_, _, t), opt)| opt && matches!(self.cx.kind(*t), TyKind::Option(_)))
            .collect()
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

    /// [`format_fields`](Self::format_fields) for a type with optional fields: which fields
    /// show, and so where the separators go, is decided at run time. `written` is a Bool local,
    /// true once a field was printed; the opening text is the first field's separator.
    #[allow(clippy::too_many_arguments)] // the arguments of `format_fields`, plus the flags
    fn format_fields_optional(
        &mut self,
        buf: &Operand,
        name: Option<String>,
        place: &Place,
        ty: TyId,
        shown: &[(u32, String, TyId)],
        optional: &[bool],
        child: &Operand,
    ) {
        let open = match &name {
            Some(n) => format!("{n} {{ "),
            None => "{ ".into(),
        };
        let written = self.temp(Ty::Bool);
        let no = Operand::Const(vir::Const::Bool(false), Ty::Bool);
        self.assign(Place::local(written), Rvalue::Use(no));
        for ((index, n, t), opt) in shown.iter().zip(optional) {
            let fp = self.field_place(place, ty, *index);
            let skip = self.new_block();
            if *opt {
                let present = self.field_present(&fp, ty, *index, *t);
                let print = self.new_block();
                self.branch(present, print, skip);
                self.switch_to(print);
            }
            let (first, rest, join) = (self.new_block(), self.new_block(), self.new_block());
            self.branch(Operand::Copy(Place::local(written)), rest, first);
            self.switch_to(first);
            self.push_text(buf, &format!("{open}{n}: "));
            self.goto(join);
            self.switch_to(rest);
            self.push_text(buf, &format!(", {n}: "));
            self.goto(join);
            self.switch_to(join);
            self.assign(Place::local(written), Rvalue::Use(FnLower::ctrue()));
            self.format_nested(buf, &fp, *t, child);
            self.goto(skip);
            self.switch_to(skip);
        }
        let (some, none, done) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(Operand::Copy(Place::local(written)), some, none);
        self.switch_to(some);
        self.push_text(buf, " }");
        self.goto(done);
        self.switch_to(none);
        let empty = match &name {
            Some(n) => format!("{n} {{}}"),
            None => "{}".into(),
        };
        self.push_text(buf, &empty);
        self.goto(done);
        self.switch_to(done);
    }

    /// Whether optional field `index` (at `fp`, of type `t`) of `ty` is present: its presence
    /// flag for `a?: T | null` (a present `null` shows), else whether it is not `null`.
    fn field_present(&mut self, fp: &Place, ty: TyId, index: u32, t: TyId) -> Operand {
        match self.cx.presence_slot(ty, index) {
            Some(slot) => {
                let mut f = fp.clone();
                if let Some(Proj::Field(x)) = f.proj.last_mut() {
                    *x = slot;
                }
                Operand::Copy(f)
            }
            None => self.option_is_some(fp, t),
        }
    }

    /// A struct without fields (zero-sized).
    fn is_empty_struct(&self, t: TyId) -> bool {
        matches!(self.cx.kind(t), TyKind::Adt(d, _)
            if matches!(self.cx.hir.def(d), velt_sema::hir::Def::Adt(a)
                if a.kind == AdtKind::Struct && a.fields.is_empty()))
    }
}
