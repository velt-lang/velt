//! `ToString` and string concatenation chains. Sema lowers a template literal (and `a + b + c`)
//! to a left fold of `StrConcat` with `ToString` around non-string parts; here the whole tree is
//! flattened into one builder, and the filled builder is the result (same layout as `VeltStr`,
//! no `finish` call). With string parts and only scalar formatted ones, every part is evaluated
//! first (JS order; each held against the parts after it) and the builder is sized once from
//! their byte lengths: one allocation, however long a part is. Otherwise it reserves an
//! estimate and pushes every part as soon as it is evaluated (later parts cannot change what an
//! earlier part contributed). Non-string parts are appended by the shared format glue, so `${x}`
//! is what `console.log(x)` prints, except for objects, which are written as JS's `String(x)`
//! writes them (`[object Object]`, js_string.rs).

use velt_sema::hir::{self, Intrinsic, TyId, TyKind};

use super::rt::Rt;
use super::sequence::{may_move_local, root_local, Later};
use super::{cint, FnLower};
use crate::vir::{BinOp, Operand, Place, Ty, STR_AGG};

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
            let va = self.expr_held(a, Later::of(b));
            let vb = self.expr(b);
            let pa = self.operand_addr(va, STR);
            let pb = self.operand_addr(vb, STR);
            return self.concat(pa, pb, ty);
        }
        self.build_parts(&parts, ty)
    }

    /// Evaluate `parts` in order into one fresh builder: an owned string of type `ty`.
    pub(super) fn build_parts(&mut self, parts: &[Part], ty: TyId) -> Operand {
        if self.sized_once(parts) {
            return self.build_sized(parts, ty);
        }
        let cap = self.estimate_all(parts);
        let (buf, bp) = self.new_strbuf(cap);
        // Owned from the start: a part that throws or returns early frees the partial text.
        self.own_temp(buf, ty);
        self.push_parts(&bp, parts);
        Operand::Copy(Place::local(buf))
    }

    /// Can the builder be sized once from the parts' values: is there a string part (whose
    /// length is known only once it is evaluated), and is every formatted part a number or a
    /// boolean (a copy, so it can be formatted after the parts after it ran)?
    fn sized_once(&mut self, parts: &[Part]) -> bool {
        let mut strings = false;
        for p in parts {
            match p {
                Part::Text(_) => {}
                Part::Str(e) => {
                    // A part that never finishes (`fail()`) has no string to measure.
                    let t = self.sub(e.ty);
                    if matches!(self.cx.kind(t), TyKind::Never) {
                        return false;
                    }
                    strings = true;
                }
                Part::Format(e) => {
                    let t = self.sub(e.ty);
                    if !matches!(
                        self.cx.kind(t),
                        TyKind::Int(_) | TyKind::Float(_) | TyKind::Bool
                    ) {
                        return false;
                    }
                }
            }
        }
        strings
    }

    /// [`build_parts`](Self::build_parts) with one allocation: every part is evaluated first, in
    /// order (each held against what the parts after it may do), then the builder gets the
    /// static text's length plus the strings' byte lengths plus the widest a number can be, and
    /// the parts are appended. A long string part (a list of rows) no longer regrows the builder.
    fn build_sized(&mut self, parts: &[Part], ty: TyId) -> Operand {
        let mut later = vec![Later::default(); parts.len()];
        let mut acc = Later::default();
        for (i, p) in parts.iter().enumerate().rev() {
            later[i] = acc;
            if let Part::Str(e) | Part::Format(e) = p {
                let l = Later::of(e);
                acc.locals |= l.locals;
                acc.memory |= l.memory;
            }
        }
        // A string part is read only when the builder is filled, after every later part ran: if
        // a later part may move the variable it reads (`${s}|${take(s)}`, which frees `s`), it is
        // held as if that part wrote the variable.
        for i in 0..parts.len() {
            let Part::Str(e) = &parts[i] else { continue };
            let Some(root) = root_local(e) else { continue };
            let moved = parts[i + 1..].iter().any(|p| match p {
                Part::Str(x) | Part::Format(x) => may_move_local(x, root),
                Part::Text(_) => false,
            });
            later[i].locals |= moved;
        }
        let mut values = Vec::with_capacity(parts.len());
        for (i, p) in parts.iter().enumerate() {
            values.push(match p {
                Part::Text(_) => None,
                Part::Str(e) | Part::Format(e) => Some(self.expr_held(e, later[i])),
            });
        }
        let mut fixed = 0u64;
        let mut cap: Option<Operand> = None;
        for (p, v) in parts.iter().zip(&values) {
            match (p, v) {
                (Part::Text(s), _) => fixed += s.len() as u64,
                (Part::Format(e), _) => {
                    let t = self.sub(e.ty);
                    fixed += match self.cx.kind(t) {
                        TyKind::Bool => 5,
                        // `-0.0000032851837118293624`: JS's longest numbers are 25 bytes.
                        TyKind::Float(_) => 25,
                        _ => 20,
                    };
                }
                (Part::Str(_), Some(v)) => {
                    let place = self.operand_place(v.clone(), STR);
                    let n = self.str_bytes(&place);
                    cap = Some(match cap {
                        Some(c) => self.u64_op(BinOp::Add, c, n),
                        None => n,
                    });
                }
                (Part::Str(_), None) => {}
            }
        }
        let fixed = cint(fixed as i128, Ty::U64);
        let cap = match cap {
            Some(c) => self.u64_op(BinOp::Add, c, fixed),
            None => fixed,
        };
        let (buf, bp) = self.new_strbuf_sized(cap);
        self.own_temp(buf, ty);
        for (p, v) in parts.iter().zip(values) {
            match (p, v) {
                (Part::Text(s), _) => self.push_text(&bp, s),
                (Part::Format(e), Some(v)) => {
                    let t = self.sub(e.ty);
                    self.push_scalar(&bp, v, t);
                }
                (Part::Str(_), Some(v)) => {
                    let a = self.operand_addr(v, STR);
                    self.push_str(&bp, a);
                }
                _ => {}
            }
        }
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
                        // A number is most often a count or an index (`${x},${y}`): as short as
                        // an integer, so a short template stays inline.
                        TyKind::Int(_) | TyKind::Float(_) => 8,
                        TyKind::Bool => 5,
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
                if !self.push_js_object(buf, &p, t) {
                    self.format_top(buf, &p, t);
                }
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
