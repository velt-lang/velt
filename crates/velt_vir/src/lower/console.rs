//! `console.log` / `console.error`. Lines made only of numbers, bools and strings are written
//! piecewise straight to the stream (the fast path: no builder, no allocation), a number in one
//! write with the space or newline after it; any other line is formatted by the shared format
//! glue (glue/format.rs) into one builder and written at once.

use velt_sema::hir::{self, LitValue, TyId, TyKind};

use super::hook_calls::{self as hooks, ty, Construct, HookTy};
use super::rt::Rt;
use super::{cint, unit, FnLower};
use crate::vir::{Operand, Place, Ty, STR_AGG};

/// Initial capacity of a `console.log` line builder: most lines fit without growing.
const LINE_CAP: u64 = 64;

impl FnLower<'_, '_> {
    /// Write a literal text to `stream`.
    pub(super) fn write_text(&mut self, stream: &Operand, text: &str) {
        let s = self.str_lit(text);
        let a = self.operand_addr(s, Ty::Agg(STR_AGG));
        self.call_rt(Rt::WriteStr, vec![stream.clone(), a], None);
    }

    /// `console.log(a, b)`: evaluate every argument first (JS order), then write
    /// `<a> <b>\n` to `stream` (1 = stdout, 2 = stderr). A line to stdout first marks the thread
    /// as having written (std's `stdoutWritten`), so that a hand-off publishes the thread's
    /// buffer before another task can print (#2771).
    pub(super) fn print(&mut self, stream: i128, args: &[hir::Expr]) -> Operand {
        let mut vals = vec![];
        for (a, later) in args.iter().zip(self.later_each(args)) {
            let v = self.expr_held(a, later);
            let t = self.sub(a.ty);
            vals.push((v, t));
        }
        if stream == 1 {
            _ = hooks::stdout_written(self).emit(self);
        }
        let stream = cint(stream, Ty::U32);
        let direct = |lw: &Self, t: TyId| lw.writes_directly(t) || lw.option_scalar(t).is_some();
        if vals.iter().all(|&(_, t)| direct(self, t)) {
            self.print_direct(stream, vals);
        } else {
            self.print_formatted(stream, vals);
        }
        unit()
    }

    /// Types the fast path writes without the format glue.
    fn writes_directly(&self, t: TyId) -> bool {
        matches!(
            self.cx.kind(t),
            TyKind::Int(_)
                | TyKind::Float(_)
                | TyKind::Bool
                | TyKind::Str
                | TyKind::Unit
                | TyKind::Never
                | TyKind::Literal(LitValue::Str(_) | LitValue::Int(..) | LitValue::Bool(_))
        )
    }

    fn print_direct(&mut self, stream: Operand, vals: Vec<(Operand, TyId)>) {
        let count = vals.len();
        for (k, (v, t)) in vals.into_iter().enumerate() {
            let end = if k + 1 == count { b'\n' } else { b' ' };
            if let Some((p, n)) = self.option_scalar(t) {
                self.write_option(&stream, v, t, p, n, end);
                continue;
            }
            match self.cx.kind(t) {
                TyKind::Str => {
                    let p = self.operand_addr(v, Ty::Agg(STR_AGG));
                    self.call_rt(Rt::WriteStr, vec![stream.clone(), p], None);
                }
                // A diverging argument: the print is unreachable.
                TyKind::Never => {}
                TyKind::Unit => self.write_text(&stream, "undefined"),
                TyKind::Literal(LitValue::Str(s)) => self.write_text(&stream, &s),
                TyKind::Literal(LitValue::Int(_, n)) => self.write_text(&stream, &n.to_string()),
                TyKind::Literal(LitValue::Bool(b)) => {
                    self.write_text(&stream, if b { "true" } else { "false" })
                }
                TyKind::Bool => self.call_rt(Rt::WriteBool, vec![stream.clone(), v], None),
                // A number is written with the byte after it, in one write.
                _ => {
                    self.write_number(stream.clone(), v, t, end);
                    continue;
                }
            }
            self.write_byte(&stream, end);
        }
        if count == 0 {
            self.write_byte(&stream, b'\n');
        }
    }

    /// Format the whole line into one builder, write it, free the builder.
    fn print_formatted(&mut self, stream: Operand, vals: Vec<(Operand, TyId)>) {
        let (_, bp) = self.new_strbuf(LINE_CAP);
        for (k, (v, t)) in vals.into_iter().enumerate() {
            if k > 0 {
                self.push_text(&bp, " ");
            }
            match self.cx.kind(t) {
                TyKind::Never => continue,
                TyKind::Float(_) => {
                    self.push_inspect_float(&bp, v, t);
                    continue;
                }
                _ => {}
            }
            let p = self.place_of(v, t);
            self.log_top(&bp, &p, t);
        }
        self.push_text(&bp, "\n");
        self.call_rt(Rt::WriteStr, vec![stream, bp.clone()], None);
        self.call_rt(Rt::StrbufDrop, vec![bp], None);
    }

    /// Write the number, string or boolean at `place` (of type `ty`) as `console.log` does, and
    /// the byte `end` after it: a number in one write with it.
    pub(super) fn write_payload(&mut self, stream: &Operand, place: Place, ty: TyId, end: u8) {
        match self.cx.kind(ty) {
            TyKind::Str => {
                let a = self.addr(place);
                self.call_rt(Rt::WriteStr, vec![stream.clone(), a], None);
            }
            TyKind::Bool => {
                let v = Operand::Copy(place);
                self.call_rt(Rt::WriteBool, vec![stream.clone(), v], None);
            }
            _ => return self.write_number(stream.clone(), Operand::Copy(place), ty, end),
        }
        self.write_byte(stream, end);
    }

    pub(super) fn write_byte(&mut self, stream: &Operand, b: u8) {
        self.call_rt(
            Rt::WriteByte,
            vec![stream.clone(), cint(b as i128, Ty::U8)],
            None,
        );
    }

    /// A number in JS formatting and the byte after it, through std's writers
    /// (std/lang/hooks/number_text.velt, number_write.velt). A float goes to the runtime's printer
    /// while sema leaves the float writer unchecked (velt_common `ON_DEMAND_WRITE`).
    fn write_number(&mut self, stream: Operand, v: Operand, t: TyId, end: u8) {
        let (s, from) = (ty::U32::arg(stream.clone()), self.cx.ty(t));
        let e = ty::U8::arg(cint(end as i128, Ty::U8));
        let c = Construct::Format;
        match self.cx.kind(t) {
            TyKind::Float(_) => {
                let v = self.float_as_f64(v, t);
                match hooks::number_write(self, s, ty::F64::arg(v.clone()), e) {
                    Some(call) => _ = call.emit(self),
                    None => {
                        self.call_rt(Rt::WriteF64, vec![stream.clone(), v], None);
                        self.write_byte(&stream, end);
                    }
                }
            }
            TyKind::Int(it) if !it.is_signed() => {
                let v = self.cast_to(v, from, Ty::U64);
                _ = hooks::uint_write(self, c, s, ty::U64::arg(v), e).emit(self);
            }
            _ => {
                let v = self.cast_to(v, from, Ty::I64);
                _ = hooks::int_write(self, c, s, ty::I64::arg(v), e).emit(self);
            }
        }
    }
}
