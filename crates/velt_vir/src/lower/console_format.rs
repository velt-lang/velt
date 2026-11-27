//! `console.log("%s=%d", k, v)` with a format string known when compiling (#3662): the literal
//! is split as Node's `util.format` reads it (velt_sema's `console_format::split`, the splitter
//! sema converted the arguments with), and each piece is written in place: a text as it is, an
//! argument by its specifier. Sema has converted `%d`, `%i`, `%f` and `%j` arguments to
//! numbers and strings, and a `%s` of an object with a `toString()` of its own to its text; here
//! a `%s` of any other object is inspected at depth 0, `%o` and `%O` inspect as `console.log`
//! does, and `%c` writes nothing. A line of numbers, strings and booleans is written piecewise to
//! the stream, as console.rs writes one; any other is formatted into one builder.

use rustc_hash::FxHashSet;
use velt_sema::console_format::{split, Piece, BUILTIN_CLASSES};
use velt_sema::hir::{self, LitValue, TyId, TyKind};

use super::hooks::Hook;
use super::FnLower;
use crate::lower::rt::Rt;
use crate::vir::{Operand, Place};

/// A part of a formatted line.
enum Item {
    Text(String),
    /// An argument, the specifier that took it (`None`: a leftover one).
    Value(Option<u8>, Operand, TyId),
}

impl FnLower<'_, '_> {
    /// The line `fmt` (the first argument's literal text) followed by `rest` writes, when its
    /// specifiers format it; `None` when it prints as without specifiers.
    fn format_items(fmt: &str, rest: Vec<(Operand, TyId)>) -> Option<Vec<Item>> {
        let parts = split(fmt, rest.len())?;
        let mut vals: Vec<Option<(Operand, TyId)>> = rest.into_iter().map(Some).collect();
        let mut items: Vec<Item> = vec![];
        let push_text = |items: &mut Vec<Item>, s: &str| match items.last_mut() {
            Some(Item::Text(t)) => t.push_str(s),
            _ => items.push(Item::Text(s.to_string())),
        };
        for p in &parts.pieces {
            match *p {
                Piece::Text(s) => push_text(&mut items, s),
                // `%c` takes its argument (evaluated) and writes nothing.
                Piece::Arg(b'c', _) => {}
                Piece::Arg(c, i) => {
                    let (v, t) = vals[i - 1].take()?;
                    items.push(Item::Value(Some(c), v, t));
                }
            }
        }
        for v in vals.iter_mut().skip(parts.used) {
            let (v, t) = v.take()?;
            push_text(&mut items, " ");
            items.push(Item::Value(None, v, t));
        }
        push_text(&mut items, "\n");
        Some(items)
    }

    /// Print `vals` (the first one a string literal typed `fmt`) to `stream` with its format
    /// specifiers; the values back (nothing written) when the line has none that format it.
    pub(super) fn print_with_format(
        &mut self,
        stream: &Operand,
        fmt: &str,
        mut vals: Vec<(Operand, TyId)>,
    ) -> Result<(), Vec<(Operand, TyId)>> {
        if vals.len() < 2 || split(fmt, vals.len() - 1).is_none() {
            return Err(vals);
        }
        let rest = vals.split_off(1);
        let Some(items) = Self::format_items(fmt, rest) else {
            super::ice("console format: an argument taken twice");
        };
        if self.format_direct(&items) {
            self.write_items(stream, items);
        } else {
            self.build_items(stream, items);
        }
        Ok(())
    }

    /// Can `items` be written piecewise: every value a number, string or boolean written as
    /// `console.log` writes it, and every number followed by a text starting with an ASCII byte
    /// (written with the number, in one write)?
    fn format_direct(&self, items: &[Item]) -> bool {
        items.iter().enumerate().all(|(k, it)| match it {
            Item::Text(_) => true,
            Item::Value(spec, _, t) => {
                let direct = match spec {
                    Some(b'o' | b'O') => matches!(
                        self.cx.kind(*t),
                        TyKind::Int(_)
                            | TyKind::Float(_)
                            | TyKind::Bool
                            | TyKind::Literal(LitValue::Int(..) | LitValue::Bool(_))
                    ),
                    _ => self.writes_directly(*t),
                };
                let number = matches!(self.cx.kind(*t), TyKind::Int(_) | TyKind::Float(_));
                let ascii_next = matches!(
                    items.get(k + 1),
                    Some(Item::Text(s)) if s.as_bytes().first().is_some_and(u8::is_ascii)
                );
                direct && (!number || ascii_next)
            }
        })
    }

    /// Write `items` piecewise to `stream` ([`Self::format_direct`]).
    fn write_items(&mut self, stream: &Operand, items: Vec<Item>) {
        let mut skip_first = false;
        let mut it = items.into_iter().peekable();
        while let Some(item) = it.next() {
            match item {
                Item::Text(s) => {
                    let s = if skip_first { &s[1..] } else { &s[..] };
                    skip_first = false;
                    if !s.is_empty() {
                        self.write_text(stream, s);
                    }
                }
                Item::Value(_, v, t) => {
                    // A number is written with the (ASCII, `format_direct`) byte after it.
                    let end = match it.peek() {
                        Some(Item::Text(s)) => s.as_bytes().first().copied(),
                        _ => None,
                    };
                    skip_first = self.write_value(stream, v, t, end);
                }
            }
        }
    }

    /// Write one value of a direct line; true when the byte `end` after it was written with it
    /// (a number), so the next text starts after its first byte.
    fn write_value(&mut self, stream: &Operand, v: Operand, t: TyId, end: Option<u8>) -> bool {
        match self.cx.kind(t) {
            TyKind::Int(_) | TyKind::Float(_) => {
                let Some(end) = end else {
                    super::ice("console format: a number without a byte after it");
                };
                self.write_number(stream.clone(), v, t, end);
                true
            }
            TyKind::Never => false,
            TyKind::Str => {
                let p = self.operand_addr(v, crate::vir::Ty::Agg(crate::vir::STR_AGG));
                self.write_str(stream, p);
                false
            }
            TyKind::Unit => {
                self.write_text(stream, "undefined");
                false
            }
            TyKind::Literal(LitValue::Str(s)) => {
                self.write_text(stream, &s);
                false
            }
            TyKind::Literal(LitValue::Int(it, n)) => {
                self.write_text(stream, &super::glue::int_literal_text(it, n));
                false
            }
            TyKind::Literal(LitValue::Bool(b)) => {
                self.write_text(stream, if b { "true" } else { "false" });
                false
            }
            TyKind::Bool => {
                self.write_bool(stream, v);
                false
            }
            _ => super::ice("console format: a value that is not written directly"),
        }
    }

    /// Format `items` into one builder, write it, free the builder.
    fn build_items(&mut self, stream: &Operand, items: Vec<Item>) {
        let (_, bp) = self.new_strbuf(super::console::LINE_CAP);
        for it in items {
            match it {
                Item::Text(s) => self.push_text(&bp, &s),
                Item::Value(spec, v, t) => {
                    if matches!(self.cx.kind(t), TyKind::Never) {
                        continue;
                    }
                    let p = self.place_of(v, t);
                    match spec {
                        Some(b'o' | b'O') => self.format_top_inspect(&bp, &p, t),
                        Some(b's') => self.push_spec_s(&bp, &p, t),
                        _ => self.log_top(&bp, &p, t),
                    }
                }
            }
        }
        self.write_str(stream, bp.clone());
        self.call_rt(Rt::StrbufDrop, vec![bp], None);
    }

    /// Has the class `ty` a `toString()` hook that a class other than a JavaScript built-in
    /// declares (Node's `hasBuiltInToString` is false, velt_sema `console_format`)?
    pub(super) fn own_to_string(&mut self, ty: TyId) -> bool {
        let Some(m) = self.hook(ty, Hook::ToString) else {
            return false;
        };
        let Some(owner) = self.cx.fn_def(m).self_ty else {
            return false;
        };
        let name = self.cx.type_name(owner);
        let name = name.split('<').next().unwrap_or_default();
        !BUILTIN_CLASSES.contains(&name)
    }

    /// Has the class `ty` (or a base class) a `[Symbol.toPrimitive]` method that no JavaScript
    /// built-in declares? Node's `hasBuiltInToString` is false for it, so `%s` writes it with
    /// `String(x)`.
    pub(super) fn own_to_primitive(&self, ty: TyId) -> bool {
        let cx = &*self.cx;
        let classes = cx.to_primitive_classes.get_or_init(|| {
            let mut out = FxHashSet::default();
            for def in &cx.hir.defs {
                let hir::Def::Fn(f) = def else { continue };
                if !f.name.ends_with(".[Symbol.toPrimitive]") {
                    continue;
                }
                if let Some(&TyKind::Adt(d, _)) = f.self_ty.map(|t| cx.raw_kind(t)) {
                    out.insert(d);
                }
            }
            out
        });
        let mut t = ty;
        for _ in 0..64 {
            let TyKind::Adt(d, _) = self.cx.kind(t) else {
                return false;
            };
            if !self.cx.is_class(t) {
                return false;
            }
            let a = self.cx.adt_def(d);
            if classes.contains(&d) {
                return !BUILTIN_CLASSES.contains(&a.name.as_str());
            }
            match a.base {
                Some(b) => t = b,
                None => return false,
            }
        }
        false
    }

    /// `%s` of the value at `place`: a primitive as `String(x)` writes it (a number as
    /// `console.log` does, `-0`), the member a nullable or a union holds likewise, and an
    /// object inspected at depth 0.
    fn push_spec_s(&mut self, buf: &Operand, place: &Place, ty: TyId) {
        if self.prints_scalar(ty) || self.enum_strings(ty).is_some() {
            return self.log_top(buf, place, ty);
        }
        match self.cx.kind(ty) {
            TyKind::Option(e, n) => {
                let (some_bb, none_bb, done) =
                    (self.new_block(), self.new_block(), self.new_block());
                let some = self.option_is_some(place, ty);
                self.branch(some, some_bb, none_bb);
                self.switch_to(none_bb);
                self.push_text(buf, n.word());
                self.goto(done);
                self.switch_to(some_bb);
                let payload = self.some_payload(place, ty);
                self.push_spec_s(buf, &payload, e);
                self.goto(done);
                self.switch_to(done);
            }
            TyKind::Adt(..) if self.cx.is_union(ty) && !self.cx.is_json_value(ty) => {
                self.for_each_variant(place, ty, |lw, v, parts| {
                    if let Some(l) = lw.variant_literal(ty, v) {
                        lw.push_literal(buf, &l, false);
                    }
                    for (pp, pt) in parts {
                        lw.push_spec_s(buf, &pp, pt);
                    }
                });
            }
            // A class instance with a `toString()` of its own (a member of a nullable or a
            // union, or a type parameter's value: sema calls it where it knows the class).
            _ if self.own_to_string(ty) => {
                self.push_to_string(buf, place, ty);
            }
            _ => {
                let depth = Self::shallow_depth();
                self.format_top_inspect_at(buf, place, ty, &depth);
            }
        }
    }
}
