//! `console.log` / `console.error`. Lines made only of numbers, bools and strings are written
//! piecewise straight to the stream (the fast path: no builder, no allocation); any other line
//! is formatted by the shared format glue (glue/format.rs) into one builder and written at once.

use velt_sema::hir::{self, LitValue, TyId, TyKind};

use super::rt::Rt;
use super::sequence::later_each;
use super::{cint, unit, FnLower};
use crate::vir::{Operand, Ty, STR_AGG};

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
    /// `<a> <b>\n` to `stream` (1 = stdout, 2 = stderr).
    pub(super) fn print(&mut self, stream: i128, args: &[hir::Expr]) -> Operand {
        let stream = cint(stream, Ty::U32);
        let mut vals = vec![];
        for (a, later) in args.iter().zip(later_each(args)) {
            let v = self.expr_held(a, later);
            let t = self.sub(a.ty);
            vals.push((v, t));
        }
        if vals.iter().all(|&(_, t)| self.writes_directly(t)) {
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
        for (k, (v, t)) in vals.into_iter().enumerate() {
            if k > 0 {
                self.write_byte(&stream, b' ');
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
                _ => self.write_scalar(stream.clone(), v, t),
            }
        }
        self.write_byte(&stream, b'\n');
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

    fn write_byte(&mut self, stream: &Operand, b: u8) {
        self.call_rt(
            Rt::WriteByte,
            vec![stream.clone(), cint(b as i128, Ty::U8)],
            None,
        );
    }

    /// Scalars (ints, floats, bool) in JS formatting.
    fn write_scalar(&mut self, stream: Operand, v: Operand, ty: TyId) {
        let from = self.cx.ty(ty);
        match self.cx.kind(ty) {
            TyKind::Float(_) => {
                let v = self.cast_to(v, from, Ty::F64);
                self.call_rt(Rt::WriteF64, vec![stream, v], None);
            }
            TyKind::Bool => self.call_rt(Rt::WriteBool, vec![stream, v], None),
            TyKind::Int(it) if !it.is_signed() => {
                let v = self.cast_to(v, from, Ty::U64);
                self.call_rt(Rt::WriteU64, vec![stream, v], None);
            }
            _ => {
                let v = self.cast_to(v, from, Ty::I64);
                self.call_rt(Rt::WriteI64, vec![stream, v], None);
            }
        }
    }
}
