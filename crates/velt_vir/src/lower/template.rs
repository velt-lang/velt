//! `ToString` and string concatenation chains. Sema lowers a template literal (and `a + b + c`)
//! to a left fold of `StrConcat` with `ToString` around non-string parts; here the whole tree is
//! flattened into one builder: reserve an estimate, push every part as soon as it is evaluated
//! (JS order — later parts cannot change what an earlier part contributed), and the filled
//! builder is the result (same layout as `VeltStr`, no `finish` call). Non-string parts are
//! appended by the shared format glue, so `${x}` is exactly what `console.log(x)` prints.

use velt_sema::hir::{self, Intrinsic, TyId, TyKind};

use super::rt::Rt;
use super::FnLower;
use crate::vir::{Operand, Place, Ty, STR_AGG};

const STR: Ty = Ty::Agg(STR_AGG);

/// One flattened part of a concatenation chain.
pub(super) enum Part<'e> {
    /// Static text (a string literal).
    Text(&'e str),
    /// `ToString(e)` of a non-string value.
    Format(&'e hir::Expr),
    /// A string-typed expression.
    Str(&'e hir::Expr),
}

/// Append the parts of `e` (a `StrConcat` tree or a leaf) in evaluation order.
pub(super) fn flatten<'e>(e: &'e hir::Expr, out: &mut Vec<Part<'e>>) {
    match &e.kind {
        hir::ExprKind::Call {
            callee: hir::Callee::Intrinsic(Intrinsic::StrConcat),
            args,
        } if args.len() == 2 => {
            flatten(&args[0], out);
            flatten(&args[1], out);
        }
        hir::ExprKind::Call {
            callee: hir::Callee::Intrinsic(Intrinsic::ToString),
            args,
        } if args.len() == 1 => out.push(Part::Format(&args[0])),
        hir::ExprKind::Lit(hir::Lit::Str(s)) => out.push(Part::Text(s)),
        _ => out.push(Part::Str(e)),
    }
}

impl FnLower<'_, '_> {
    /// `StrConcat(a, b)`, flattened with any nested concatenations and `ToString`s.
    pub(super) fn str_concat(&mut self, a: &hir::Expr, b: &hir::Expr, ty: TyId) -> Operand {
        let mut parts = vec![];
        flatten(a, &mut parts);
        flatten(b, &mut parts);
        let ty = self.sub(ty);
        if let [Part::Str(_) | Part::Text(_), Part::Str(_) | Part::Text(_)] = parts.as_slice() {
            // Two strings: `velt_rt_str_concat` allocates exactly once, at the exact size.
            let va = self.expr(a);
            let vb = self.expr(b);
            let pa = self.operand_addr(va, STR);
            let pb = self.operand_addr(vb, STR);
            return self.concat(pa, pb, ty);
        }
        self.build_parts(&parts, ty)
    }

    /// Evaluate `parts` in order into one fresh builder: an owned string of type `ty`.
    pub(super) fn build_parts(&mut self, parts: &[Part], ty: TyId) -> Operand {
        let cap = self.estimate_all(parts);
        let (buf, bp) = self.new_strbuf(cap);
        // Owned from the start: a part that throws or returns early frees the partial text.
        self.own_temp(buf, ty);
        self.push_parts(&bp, parts);
        Operand::Copy(Place::local(buf))
    }

    /// Evaluate `parts` in order, appending each to the string at address `bp`.
    pub(super) fn push_parts(&mut self, bp: &Operand, parts: &[Part]) {
        for p in parts {
            match p {
                Part::Text(s) => self.push_text(bp, s),
                Part::Format(e) => self.push_formatted(bp, e),
                Part::Str(e) => {
                    let v = self.expr(e);
                    let a = self.operand_addr(v, STR);
                    self.push_str(bp, a);
                }
            }
        }
    }

    /// Expected byte length of the result, for the builder's initial capacity: exact for the
    /// static text, typical sizes for the rest (too small only costs one regrowth).
    fn estimate_all(&mut self, parts: &[Part]) -> u64 {
        let mut n = 0;
        for p in parts {
            n += match p {
                Part::Text(s) => s.len() as u64,
                Part::Str(_) => 16,
                Part::Format(e) => {
                    let t = self.sub(e.ty);
                    match self.cx.kind(t) {
                        TyKind::Int(_) => 8,
                        TyKind::Bool => 5,
                        TyKind::Float(_) => 12,
                        _ => 32,
                    }
                }
            };
        }
        n
    }

    /// Evaluate `e` and append its `console.log` text to the builder at `buf`.
    fn push_formatted(&mut self, buf: &Operand, e: &hir::Expr) {
        let v = self.expr(e);
        let t = self.sub(e.ty);
        match self.cx.kind(t) {
            TyKind::Int(_) | TyKind::Float(_) | TyKind::Bool => self.push_scalar(buf, v, t),
            TyKind::Never => {}
            _ => {
                let p = self.place_of(v, t);
                self.format_top(buf, &p, t);
            }
        }
    }

    /// `ToString(x)` → owned string: `velt_rt_str_from_*` for scalars, a clone for strings, the
    /// format glue into a builder for everything else.
    pub(super) fn stringify(&mut self, a: &hir::Expr, ty: TyId) -> Operand {
        let aty = self.sub(a.ty);
        let ty = self.sub(ty);
        let kind = self.cx.kind(aty);
        if !matches!(
            kind,
            TyKind::Int(_) | TyKind::Float(_) | TyKind::Bool | TyKind::Str
        ) {
            let (buf, bp) = self.new_strbuf(32);
            self.own_temp(buf, ty);
            self.push_formatted(&bp, a);
            return Operand::Copy(Place::local(buf));
        }
        let v = self.expr(a);
        let out = self.temp(STR);
        let o = self.addr(Place::local(out));
        let from = self.cx.ty(aty);
        match kind {
            TyKind::Int(it) => {
                let (w, r) = if it.is_signed() {
                    (Ty::I64, Rt::StrFromI64)
                } else {
                    (Ty::U64, Rt::StrFromU64)
                };
                let v = self.cast_to(v, from, w);
                self.call_rt(r, vec![v, o], None);
            }
            TyKind::Float(_) => {
                let v = self.cast_to(v, from, Ty::F64);
                self.call_rt(Rt::StrFromF64, vec![v, o], None);
            }
            TyKind::Bool => self.call_rt(Rt::StrFromBool, vec![v, o], None),
            _ => {
                let src = self.operand_addr(v, STR);
                self.call_rt(Rt::StrClone, vec![src, o], None);
            }
        }
        self.own_temp(out, ty);
        Operand::Copy(Place::local(out))
    }
}
