//! Objects as `console.log` prints them, like Node: `Name { a: 1, b: 'x' }` (`{ … }` for an
//! anonymous object type), `Name {}` without shown fields, `[Name]` (`[Object]`) past the depth
//! limit. Private zero-sized fields are hidden, and so is an absent optional field (`a?: T`) of an
//! object type, as Node leaves out a missing key.

use velt_sema::hir::{AdtKind, TyId, TyKind};

use crate::lower::glue::literals::inspect_key;
use crate::lower::hook_calls::{self as hooks, ty, HookTy};
use crate::lower::{cint, FnLower};
use crate::vir::{self, BinOp, Operand, Place, Proj, Rvalue, Ty, STR_AGG};

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
        let anon = matches!(self.cx.kind(ty),
            TyKind::Adt(d, _) if self.cx.adt_def(d).kind == AdtKind::Anon);
        let void = shown
            .iter()
            .find(|(_, _, t)| anon && matches!(self.cx.kind(*t), TyKind::Unit));
        if let Some((_, n, _)) = void {
            let n = n.clone();
            self.void_field_panic(&n);
        }
        if shown.is_empty() {
            let text = match &name {
                Some(n) => format!("{n} {{}}"),
                None => "{}".into(),
            };
            return self.push_text(buf, &text);
        }
        let optional = self.optional_fields(ty, &shown);
        let cut = format!("[{}]", name.as_deref().unwrap_or("Object"));
        let fields = |lw: &mut Self, name, child: &Operand| match optional.iter().any(|o| *o) {
            true => lw.format_fields_optional(buf, name, place, ty, &shown, &optional, child),
            false => lw.format_fields(buf, name, place, ty, &shown, child),
        };
        // The depth limit and the cycle check, in std where the program has them checked: the
        // cycle check only where a live type can hold itself (velt_sema `live_constructs.rs`).
        let cut_text = self.str_lit(&cut);
        let cut_text = self.operand_addr(cut_text, Ty::Agg(STR_AGG));
        let tracked = match &obj {
            Some(p) => {
                let id = self.rvalue_temp(Ty::U64, Rvalue::Cast(p.clone(), Ty::U64));
                let (b, c) = (ty::Str::arg(buf.clone()), ty::Str::arg(cut_text.clone()));
                hooks::open_object(self, b, c, ty::U64::arg(id), ty::U32::arg(depth.clone()))
            }
            None => None,
        };
        let open = match tracked {
            Some(open) => Some((open.emit(self).op(), true)),
            None => {
                let (b, c) = (ty::Str::arg(buf.clone()), ty::Str::arg(cut_text));
                let plain = hooks::open_plain(self, b, c, ty::U32::arg(depth.clone()));
                plain.map(|open| (open.emit(self).op(), false))
            }
        };
        let Some((open, tracked)) = open else {
            return self.within_depth(buf, depth, &cut, obj.clone(), |lw, child| match obj {
                Some(p) => lw.format_once(buf, p, |lw| fields(lw, name, &child)),
                None => fields(lw, name, &child),
            });
        };
        let (body, done) = (self.new_block(), self.new_block());
        self.branch(open, body, done);
        self.switch_to(body);
        let child = self.rvalue_temp(
            Ty::U32,
            Rvalue::Binary(BinOp::Add, depth.clone(), cint(1, Ty::U32)),
        );
        fields(self, name, &child);
        if tracked {
            self.inspect_leave(buf);
        }
        self.goto(done);
        self.switch_to(done);
    }

    /// Panic: the object (of an object type) has a field that is `void` after substitution
    /// (velt_sema `void_fields`, #1411), which keeps no value, so what the program wrote to it
    /// (`ref: null as any`) can't be shown as Node shows it. Printing or JSON-writing it through
    /// a generic or an `any`, or reading the field through an `any`, stops here rather than
    /// showing `undefined`.
    pub(in crate::lower) fn void_field_panic(&mut self, name: &str) {
        self.panic_msg(&format!(
            "TypeError: the field `{name}` is `void` here and keeps no value, so it can't be read \
             or shown through a generic or an `any` yet (#1411)"
        ));
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
        // A field held out of line (`FieldDef::cell`) shows while its cell is there.
        let flags: Vec<bool> = shown
            .iter()
            .map(|(i, _, _)| def.fields[*i as usize].optional || def.fields[*i as usize].cell)
            .collect();
        shown
            .iter()
            .zip(flags)
            .map(|((_, _, t), opt)| {
                opt && (matches!(self.cx.kind(*t), TyKind::Option(..)) || self.cx.is_dynamic(*t))
            })
            .collect()
    }

    /// The fields an object prints: (index, name, type). ES private fields (`#x`) are hidden, as
    /// Node hides them, and so are private zero-sized fields (a marker such as std's
    /// `runtime: RuntimeHandle`); other private fields show, as Node shows a TypeScript
    /// `private` field. A hidden field (`FieldDef::hidden`, a non-enumerable property) is hidden.
    pub(super) fn shown_fields(&mut self, ty: TyId) -> Vec<(u32, String, TyId)> {
        let TyKind::Adt(d, _) = self.cx.kind(ty) else {
            crate::lower::ice("field format of a non-struct type")
        };
        let all: Vec<(u32, String, TyId, bool)> = self
            .cx
            .adt_def(d)
            .fields
            .iter()
            .map(|f| (f.name.clone(), f.private, f.hidden))
            .zip(self.cx.adt_field_tys(ty))
            .enumerate()
            .filter(|(_, ((_, _, hidden), _))| !hidden)
            .map(|(i, ((n, private, _), t))| (i as u32, n, t, private))
            .collect();
        // An object literal's accessor fields show as the property each stands for.
        let shown: Vec<(u32, String, TyId)> = all
            .into_iter()
            .filter(|(_, n, t, private)| {
                !n.starts_with('#') && !(*private && self.is_empty_struct(*t))
            })
            .filter_map(|(i, n, t, _)| self.field_prop(d, &n).map(|p| (i, p, t)))
            .collect();
        // JavaScript's order: array-index names (`'404'`) first, ascending (#756).
        let names: Vec<&str> = shown.iter().map(|(_, n, _)| n.as_str()).collect();
        let order = velt_sema::property_order::js_key_order(&names);
        order
            .into_iter()
            .map(|i| {
                let (index, n, t) = &shown[i];
                // Node 26 writes a symbol key as `Symbol(d): v`, without brackets.
                let key = match velt_sema::property_order::symbol_key(n) {
                    Some(k) => symbol_key_text(k.trim_start_matches('[').trim_end_matches(']')),
                    None => inspect_key(n),
                };
                (*index, key, *t)
            })
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
            pending.push_str(sep);
            self.push_key(buf, &pending, n);
            pending.clear();
            match self.accessor_label(ty, *index) {
                Some(label) => self.push_text(buf, label),
                // An error's `message` shows what a read gives (`error_message.rs`). Only a
                // class has it, and a class's fields are never optional here.
                None if n == "message" && self.error_message_hooked(ty) => {
                    self.format_error_message(buf, place, child);
                }
                // A weak field shows its target, or `null` once it is gone (weak_fields.rs).
                None if self.cx.weak_field(ty, *index).is_some() => {
                    let fp = self.field_place(place, ty, *index);
                    let v = self.weak_value(fp, *t);
                    let vp = self.place_of(v, *t);
                    self.format_nested(buf, &vp, *t, child);
                    self.drop_glue(vp, *t);
                }
                None => {
                    let fp = self.field_place(place, ty, *index);
                    self.format_nested(buf, &fp, *t, child);
                }
            }
        }
        self.push_text(buf, " }");
    }

    /// `before`, then the key `n` and its `: `. A symbol key whose description holds what line
    /// breaking reads as structure (a quote, a bracket, `, `) is kept as one piece, as a custom
    /// inspect's text is (`velt_rt_strbuf_inspect_atom`).
    fn push_key(&mut self, buf: &Operand, before: &str, n: &str) {
        let special = |c: char| " ,:<'\"`{[()]}".contains(c);
        let kept = n
            .strip_prefix("Symbol(")
            .and_then(|d| d.strip_suffix(')'))
            .is_some_and(|d| d.contains(special));
        if !kept {
            return self.push_text(buf, &format!("{before}{n}: "));
        }
        self.push_text(buf, before);
        let start = self.builder_len(buf);
        self.push_text(buf, n);
        self.inspect_atom(buf, start);
        self.push_text(buf, ": ");
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
            self.push_key(buf, &open, n);
            self.goto(join);
            self.switch_to(rest);
            self.push_key(buf, ", ", n);
            self.goto(join);
            self.switch_to(join);
            self.assign(Place::local(written), Rvalue::Use(FnLower::ctrue()));
            match self.accessor_label(ty, *index) {
                Some(label) => self.push_text(buf, label),
                None => match self.cx.cell_field(ty, *index) {
                    Some((cell, value)) => {
                        let vp = self.cell_value_place(&fp, *t, cell);
                        self.format_nested(buf, &vp, value, child);
                    }
                    None => self.format_nested(buf, &fp, *t, child),
                },
            }
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
    /// flag for `a?: T | null` (a present `null` shows), else whether it is not `null` (`undefined`
    /// for an `a?: any`).
    fn field_present(&mut self, fp: &Place, ty: TyId, index: u32, t: TyId) -> Operand {
        match self.cx.presence_slot(ty, index) {
            Some(slot) => {
                let mut f = fp.clone();
                if let Some(Proj::Field(x)) = f.proj.last_mut() {
                    *x = slot;
                }
                Operand::Copy(f)
            }
            // `a?: any`: present unless it holds `undefined`.
            None if self.cx.is_dynamic(t) => self.any_defined(fp),
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

/// A symbol key's `Symbol(d)` as Node writes it: with the escapes of its string keys
/// (`strEscapeSequencesReplacer`: control characters, `'`, `\`, U+007F to U+009F), unquoted.
fn symbol_key_text(k: &str) -> String {
    let mut out = String::with_capacity(k.len());
    for c in k.chars() {
        match c {
            '\u{8}' => out.push_str(r"\b"),
            '\t' => out.push_str(r"\t"),
            '\n' => out.push_str(r"\n"),
            '\u{c}' => out.push_str(r"\f"),
            '\r' => out.push_str(r"\r"),
            '\'' => out.push_str(r"\'"),
            '\\' => out.push_str(r"\\"),
            '\0'..='\u{1f}' | '\u{7f}'..='\u{9f}' => {
                out.push_str(&format!(r"\x{:02X}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}
