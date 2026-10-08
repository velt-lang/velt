//! `ToString` and string concatenation chains. Sema lowers a template literal (and `a + b + c`)
//! to a left fold of `StrConcat` with `ToString` around non-string parts; here the whole tree is
//! flattened into one builder, and the filled builder is the result (same layout as `VeltStr`,
//! no `finish` call). With string parts and only scalar formatted ones, every part is evaluated
//! first (JS order; each held against the parts after it) and the builder is sized once from
//! their byte lengths: one allocation, however long a part is. When a string part is a call's
//! fresh result of 1 KiB or more that nothing else holds, its buffer becomes the builder
//! instead (grown in place, the parts before it written in front), so wrapping a page in
//! `<!DOCTYPE html>${…}` or `${…}</body>` allocates nothing. Otherwise it reserves an
//! estimate and pushes every part as soon as it is evaluated (later parts cannot change what an
//! earlier part contributed). Non-string parts are appended by the shared format glue, so `${x}`
//! is what `console.log(x)` prints, except for objects, which are written as JS's `String(x)`
//! writes them (`[object Object]`, js_string.rs).

use velt_sema::hir::{self, Intrinsic, TyId, TyKind};

use super::operand::proj;
use super::rt::Rt;
use super::sequence::{may_move_local, root_local, Later};
use super::{cint, ice, DropEntry, FnLower};
use crate::vir::{BinOp, Operand, Place, Proj, Rvalue, Ty, STR_AGG};

const STR: Ty = Ty::Agg(STR_AGG);

/// The most fresh parts a template compares to pick the one it reuses.
const MAX_FRESH: usize = 4;

/// The shortest part whose buffer a template reuses: below it, copying the part costs less
/// than checking it.
const REUSE_MIN: u64 = 1024;

/// One flattened part of a concatenation chain.
pub(super) enum Part<'e> {
    /// Static text (a string literal).
    Text(&'e str),
    /// `ToString(e)` of a non-string value.
    Format(&'e hir::Expr),
    /// A string-typed expression.
    Str(&'e hir::Expr),
}

/// Is `e` a call, whose string result is usually fresh (see `fresh_parts`)?
fn is_call(e: &hir::Expr) -> bool {
    matches!(e.kind, hir::ExprKind::Call { .. })
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
        let fresh = parts
            .iter()
            .any(|p| matches!(p, Part::Str(e) if is_call(e)));
        if let [Part::Str(_) | Part::Text(_), Part::Str(_) | Part::Text(_)] = parts.as_slice() {
            if fresh {
                // `<!DOCTYPE html>${page()}`: the page's buffer may hold both (`build_sized`).
                return self.build_sized(&parts, ty);
            }
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
                Part::Str(e) if is_call(e) => {
                    let before = self.temps_now();
                    let v = self.expr_held(e, later[i]);
                    self.drop_arguments(&before, &v);
                    Some(v)
                }
                Part::Str(e) | Part::Format(e) => Some(self.expr_held(e, later[i])),
            });
        }
        let mut fixed = 0u64;
        let mut cap: Option<Operand> = None;
        let mut lens = Vec::with_capacity(parts.len());
        for (p, v) in parts.iter().zip(&values) {
            fixed += self.fixed_width(p);
            lens.push(match (p, v) {
                (Part::Str(_), Some(v)) => {
                    let place = self.operand_place(v.clone(), STR);
                    let n = self.str_bytes(&place);
                    cap = Some(match cap {
                        Some(c) => self.u64_op(BinOp::Add, c, n.clone()),
                        None => n.clone(),
                    });
                    Some(n)
                }
                _ => None,
            });
        }
        let fixed = cint(fixed as i128, Ty::U64);
        let cap = match cap {
            Some(c) => self.u64_op(BinOp::Add, c, fixed),
            None => fixed,
        };
        let fresh = self.fresh_parts(parts, &values);
        if !fresh.is_empty() {
            return self.fill_reusing(parts, values, &lens, cap, &fresh, ty);
        }
        let (buf, bp) = self.new_strbuf_sized(cap);
        self.own_temp(buf, ty);
        for (p, v) in parts.iter().zip(values) {
            self.push_part(&bp, p, v);
        }
        Operand::Copy(Place::local(buf))
    }

    /// The bytes a part adds besides a string's length: its static text, or the widest a
    /// number or boolean can be.
    fn fixed_width(&mut self, p: &Part) -> u64 {
        match p {
            Part::Text(s) => s.len() as u64,
            Part::Format(e) => {
                let t = self.sub(e.ty);
                match self.cx.kind(t) {
                    TyKind::Bool => 5,
                    // `-0.0000032851837118293624`: JS's longest numbers are 25 bytes.
                    TyKind::Float(_) => 25,
                    _ => 20,
                }
            }
            Part::Str(_) => 0,
        }
    }

    /// Append an evaluated part to the builder at `bp`.
    fn push_part(&mut self, bp: &Operand, p: &Part, v: Option<Operand>) {
        match (p, v) {
            (Part::Text(s), _) => self.push_text(bp, s),
            (Part::Format(e), Some(v)) => {
                let t = self.sub(e.ty);
                self.push_scalar(bp, v, t);
            }
            (Part::Str(_), Some(v)) => {
                let a = self.operand_addr(v, STR);
                self.push_str(bp, a);
            }
            _ => {}
        }
    }

    /// The address of a string part's value (`v`), or of a static one's text.
    fn part_addr(&mut self, p: &Part, v: &Option<Operand>) -> Operand {
        let v = match (p, v) {
            (Part::Text(s), _) => self.str_lit(s),
            (_, Some(v)) => v.clone(),
            _ => ice("a string part without a value"),
        };
        self.operand_addr(v, STR)
    }

    /// The string parts whose values are fresh: a call's result in a temporary this template
    /// owns (and drops), which nothing else can read. At most [`MAX_FRESH`] of them.
    fn fresh_parts(&self, parts: &[Part], values: &[Option<Operand>]) -> Vec<usize> {
        let owned = |p: &Place| {
            self.scopes.iter().any(|s| {
                s.drops
                    .iter()
                    .any(|d| matches!(d, DropEntry::Temp(t, _) if t == p))
            })
        };
        let mut out = vec![];
        for (i, (p, v)) in parts.iter().zip(values).enumerate() {
            let (Part::Str(e), Some(Operand::Copy(place))) = (p, v) else {
                continue;
            };
            if is_call(e) && place.proj.is_empty() && owned(place) && out.len() < MAX_FRESH {
                out.push(i);
            }
        }
        out
    }

    /// The builder of [`build_sized`](Self::build_sized) when string parts are fresh: the
    /// longest that can be reused ([`reusable_len`](Self::reusable_len)) becomes the builder,
    /// its buffer grown to `cap` with the parts before it written in front
    /// (`velt_rt_strbuf_adopt`, which checks again and may refuse), and the parts after it are
    /// appended. Otherwise a new builder gets every part, or two strings are concatenated, as
    /// without fresh parts.
    fn fill_reusing(
        &mut self,
        parts: &[Part],
        values: Vec<Option<Operand>>,
        lens: &[Option<Operand>],
        cap: Operand,
        fresh: &[usize],
        ty: TyId,
    ) -> Operand {
        let buf = self.temp(STR);
        let bp = self.addr(Place::local(buf));
        let (end, fallback) = (self.new_block(), self.new_block());
        let len_of = |k: usize| {
            lens[k]
                .clone()
                .unwrap_or_else(|| ice("a fresh part without a length"))
        };
        // The longest fresh part whose buffer the runtime may reuse (checked again there).
        let mut best = cint(0, Ty::U64);
        let mut sel = cint(0, Ty::U64);
        for &k in fresh {
            let v = values[k]
                .clone()
                .unwrap_or_else(|| ice("a fresh part without a value"));
            let place = self.operand_place(v, STR);
            let len = self.reusable_len(&place, len_of(k));
            if fresh.len() == 1 {
                best = len;
                continue;
            }
            let gt = self.rvalue_temp(
                Ty::Bool,
                Rvalue::Binary(BinOp::Gt, len.clone(), best.clone()),
            );
            let gt = self.rvalue_temp(Ty::U64, Rvalue::Cast(gt, Ty::U64));
            let mask = self.u64_op(BinOp::Sub, cint(0, Ty::U64), gt);
            best = self.select(mask.clone(), len, best);
            sel = self.select(mask, cint(k as i128, Ty::U64), sel);
        }
        let any = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Ne, best, cint(0, Ty::U64)));
        let dispatch = self.new_block();
        self.branch(any, dispatch, fallback);
        self.switch_to(dispatch);
        let chosen = (fresh.len() > 1).then_some(sel);
        for &k in fresh {
            let next = chosen.as_ref().map(|sel| {
                let is_k = self.rvalue_temp(
                    Ty::Bool,
                    Rvalue::Binary(BinOp::Eq, sel.clone(), cint(k as i128, Ty::U64)),
                );
                let (try_k, next) = (self.new_block(), self.new_block());
                self.branch(is_k, try_k, next);
                self.switch_to(try_k);
                next
            });
            let (head, owned_head) = self.head_of(&parts[..k], &values[..k], &lens[..k]);
            let part = values[k]
                .clone()
                .unwrap_or_else(|| ice("a fresh part without a value"));
            let part = self.operand_addr(part, STR);
            let r = self.temp(Ty::U8);
            let args = vec![part, head, cap.clone(), bp.clone()];
            self.call_rt(Rt::StrbufAdopt, args, Some(Place::local(r)));
            if let Some(h) = owned_head {
                self.drop_glue(Place::local(h), ty);
            }
            let ok = self.rvalue_temp(
                Ty::Bool,
                Rvalue::Binary(BinOp::Ne, Operand::Copy(Place::local(r)), cint(0, Ty::U8)),
            );
            let adopted = self.new_block();
            self.branch(ok, adopted, fallback);
            self.switch_to(adopted);
            for (p, v) in parts.iter().zip(&values).skip(k + 1) {
                self.push_part(&bp, p, v.clone());
            }
            self.goto(end);
            // The test for the next part continues here.
            if let Some(next) = next {
                self.switch_to(next);
            }
        }
        if chosen.is_some() {
            self.goto(fallback);
        }
        self.switch_to(fallback);
        if let [a @ (Part::Str(_) | Part::Text(_)), b @ (Part::Str(_) | Part::Text(_))] = parts {
            // Two strings: one exact allocation, as `str_concat` does without a fresh part.
            let a = self.part_addr(a, &values[0]);
            let b = self.part_addr(b, &values[1]);
            self.call_rt(Rt::StrConcat, vec![a, b, bp.clone()], None);
        } else {
            self.call_rt(Rt::StrbufNew, vec![cap, bp.clone()], None);
            for (p, v) in parts.iter().zip(values) {
                self.push_part(&bp, p, v);
            }
        }
        self.goto(end);
        self.switch_to(end);
        self.own_temp(buf, ty);
        Operand::Copy(Place::local(buf))
    }

    /// After a fresh part's call (`before`: the temporaries owned before it, its value `v`): drop
    /// the call's argument temporaries now rather than at the end of the statement. The call
    /// has returned and nothing reads them any more; a string the result shares with one of
    /// them (`renderToStringSync(page())` returns the page element's markup) is then the
    /// result's alone, so the template can reuse its buffer.
    fn drop_arguments(&mut self, before: &[Place], v: &Operand) {
        let Operand::Copy(keep) = v else { return };
        if !keep.proj.is_empty() || self.dead() {
            return;
        }
        let Some(scope) = self.scopes.last_mut() else {
            return;
        };
        let mut done = vec![];
        let mut i = 0;
        while i < scope.drops.len() {
            match &scope.drops[i] {
                DropEntry::Temp(t, _) if t != keep && !before.contains(t) => {
                    done.push(scope.drops.remove(i))
                }
                _ => i += 1,
            }
        }
        for d in &done {
            self.drop_entry(d);
        }
    }

    /// The temporaries the innermost scope owns now (see `drop_arguments`).
    fn temps_now(&self) -> Vec<Place> {
        let Some(scope) = self.scopes.last() else {
            return vec![];
        };
        scope
            .drops
            .iter()
            .filter_map(|d| match d {
                DropEntry::Temp(t, _) => Some(t.clone()),
                _ => None,
            })
            .collect()
    }

    /// `len` when it is at least [`REUSE_MIN`] and the string at `place` is a heap string with
    /// a buffer of its own (not a slice, a literal or an inline string), else 0. Read inline,
    /// so most parts that can't be reused cost no runtime call; whether the string is the
    /// buffer's only reference is `velt_rt_strbuf_adopt`'s to check.
    fn reusable_len(&mut self, place: &Place, len: Operand) -> Operand {
        let out = self.temp(Ty::U64);
        self.assign(Place::local(out), Rvalue::Use(cint(0, Ty::U64)));
        // First the length, which rules out the short parts most templates have with one
        // compare on a number already at hand.
        let big = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Ge, len.clone(), cint(REUSE_MIN as i128, Ty::U64)),
        );
        let (form, take, done) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(big, form, done);
        self.switch_to(form);
        let w2 = self.rvalue_temp(
            Ty::U64,
            Rvalue::Use(Operand::Copy(proj(place, Proj::Field(2)))),
        );
        // A plain heap string's `w2` is 1 to `MAX_LEN` (rt_abi.md "Strings"); an inline
        // string's or a slice's has a high bit set, a literal's is 0.
        let below = self.u64_op(BinOp::Sub, w2, cint(1, Ty::U64));
        let heap = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Lt, below, cint(i32::MAX as i128, Ty::U64)),
        );
        // Whether this is the only reference is the runtime's to check: the count is atomic.
        self.branch(heap, take, done);
        self.switch_to(take);
        self.assign(Place::local(out), Rvalue::Use(len));
        self.goto(done);
        self.switch_to(done);
        Operand::Copy(Place::local(out))
    }

    /// The text of `parts` (evaluated: `values`, string lengths `lens`) as a string for
    /// `velt_rt_strbuf_adopt`: its address, and the temporary to drop afterwards when it was
    /// built (static text alone is a literal).
    fn head_of(
        &mut self,
        parts: &[Part],
        values: &[Option<Operand>],
        lens: &[Option<Operand>],
    ) -> (Operand, Option<crate::vir::Local>) {
        if parts.iter().all(|p| matches!(p, Part::Text(_))) {
            let text: String = parts
                .iter()
                .map(|p| match p {
                    Part::Text(s) => *s,
                    _ => "",
                })
                .collect();
            let lit = self.str_lit(&text);
            return (self.operand_addr(lit, STR), None);
        }
        let mut cap = cint(0, Ty::U64);
        let mut fixed = 0;
        for (p, n) in parts.iter().zip(lens) {
            fixed += self.fixed_width(p);
            if let Some(n) = n {
                cap = self.u64_op(BinOp::Add, cap, n.clone());
            }
        }
        let cap = self.u64_op(BinOp::Add, cap, cint(fixed as i128, Ty::U64));
        // A head that may fit in 23 bytes (numbers are usually shorter than their widest)
        // starts inline, so a short one allocates nothing.
        let short = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Le, cap.clone(), cint(23 + fixed as i128, Ty::U64)),
        );
        let short = self.rvalue_temp(Ty::U64, Rvalue::Cast(short, Ty::U64));
        let mask = self.u64_op(BinOp::Sub, cint(0, Ty::U64), short);
        let cap = self.select(mask, cint(0, Ty::U64), cap);
        let (h, hp) = self.new_strbuf_sized(cap);
        for (p, v) in parts.iter().zip(values) {
            self.push_part(&hp, p, v.clone());
        }
        (hp, Some(h))
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
