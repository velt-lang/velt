//! Decoding values chosen from a fixed set of JSON scalars: literal types (`"low"`, `1`,
//! `true`; zero-sized, nothing is stored), unions of literal types (the tag is the matching
//! member), string enums (the member index) and numeric enums (the discriminant). A value
//! outside the set fails with `expected one of "low", "high"` (`expected "low"` for one).

use velt_sema::hir::{self, LitValue, TyId, TyKind};

use super::{json_quote, STR, TOKEN_FALSE, TOKEN_NUMBER, TOKEN_STRING, TOKEN_TRUE};
use crate::lower::rt::Rt;
use crate::lower::{cint, ice, FnLower};
use crate::vir::{BinOp, BlockId, Const, Local, Operand, Place, Rvalue, Terminator, Ty};

/// JSON text of a literal (`"low"`, `1`, `2.5`, `true`) for messages.
pub(super) fn literal_text(l: &LitValue) -> String {
    match l {
        LitValue::Str(s) => json_quote(s),
        LitValue::Int(_, n) => n.to_string(),
        LitValue::Float(_, bits) => {
            let f = f64::from_bits(*bits);
            if f.fract() == 0.0 && f.abs() < 1e21 {
                format!("{}", f as i128)
            } else {
                format!("{f}")
            }
        }
        LitValue::Bool(b) => b.to_string(),
    }
}

/// `"low"` for one choice, `one of "low", "high"` for several. The choices are listed by JSON
/// kind (strings, then numbers, then booleans), each kind in the type's member order: a union's
/// member order follows type interning, which a literal type the prelude happens to create
/// earlier would otherwise change (`"auto" | 0 | 1.5 | false`, not `false, "auto", 0, 1.5`).
fn choice_text(alts: &[LitValue]) -> String {
    let rank = |l: &LitValue| match l {
        LitValue::Str(_) => 0,
        LitValue::Int(..) | LitValue::Float(..) => 1,
        LitValue::Bool(_) => 2,
    };
    let mut sorted: Vec<&LitValue> = alts.iter().collect();
    sorted.sort_by_key(|l| rank(l));
    let texts: Vec<String> = sorted.into_iter().map(literal_text).collect();
    match texts.as_slice() {
        [one] => one.clone(),
        _ => format!("one of {}", texts.join(", ")),
    }
}

impl FnLower<'_, '_> {
    /// The literal values a JSON value of type `ty` is chosen from, if `ty` is a literal type,
    /// a union of literal types or a C-like enum (in member order: the index is the tag or
    /// member index).
    pub(super) fn json_choices(&mut self, ty: TyId) -> Option<Vec<LitValue>> {
        match self.cx.kind(ty) {
            TyKind::Literal(l) => Some(vec![l]),
            TyKind::Adt(d, _) if self.cx.is_union(ty) => {
                let n = self.cx.enum_def(d).variants.len() as u32;
                (0..n).map(|k| self.variant_literal(ty, k)).collect()
            }
            TyKind::Adt(d, _)
                if matches!(self.cx.hir.def(d), hir::Def::Enum(_)) && self.cx.is_c_like_enum(d) =>
            {
                let e = self.cx.enum_def(d);
                Some(
                    e.variants
                        .iter()
                        .map(|v| match &v.str_value {
                            Some(s) => LitValue::Str(s.clone()),
                            None => LitValue::Int(hir::IntTy::I64, v.discriminant as i128),
                        })
                        .collect(),
                )
            }
            _ => None,
        }
    }

    /// Decode one of `ty`'s choices (see `json_choices`) into `place`.
    pub(super) fn json_read_choice(
        &mut self,
        r: Local,
        place: &Place,
        ctx: Local,
        ty: TyId,
        fail: BlockId,
    ) {
        let alts = self
            .json_choices(ty)
            .unwrap_or_else(|| ice("JSON choice of a type without fixed values"));
        let idx = self.json_match_choice(r, ctx, &alts, fail);
        self.json_store_choice(place, ty, idx);
    }

    /// Store choice `idx` (a `U32` local, see `json_choices`) of `ty` into `place`.
    pub(super) fn json_store_choice(&mut self, place: &Place, ty: TyId, idx: Local) {
        match self.cx.kind(ty) {
            TyKind::Literal(_) => {}
            TyKind::Adt(..) if self.cx.is_union(ty) => {
                // Every member is zero-sized: the value is just the tag (`{ u32 }`), assigned
                // whole so the place is fully initialized.
                let Ty::Agg(a) = self.cx.ty(ty) else {
                    ice("union of literal types is not an aggregate")
                };
                let tag = Operand::Copy(Place::local(idx));
                self.assign(place.clone(), Rvalue::Aggregate(a, vec![tag]));
            }
            TyKind::Adt(d, _) => {
                let e = self.cx.enum_def(d);
                // A string enum's value is its member index, a numeric enum's its discriminant.
                let value = if e.variants.iter().any(|v| v.str_value.is_some()) {
                    self.cast_to(Operand::Copy(Place::local(idx)), Ty::U32, Ty::I64)
                } else {
                    let discs: Vec<i64> = e.variants.iter().map(|v| v.discriminant).collect();
                    self.json_index_to_value(idx, &discs)
                };
                let vt = self.cx.ty(ty);
                let v = self.cast_to(value, Ty::I64, vt);
                self.assign(place.clone(), Rvalue::Use(v));
            }
            k => ice(format_args!("JSON choice of {k:?}")),
        }
    }

    /// `discs[idx]` as an operand (a switch over the indexes).
    fn json_index_to_value(&mut self, idx: Local, discs: &[i64]) -> Operand {
        let out = self.temp(Ty::I64);
        self.assign(Place::local(out), Rvalue::Use(cint(0, Ty::I64)));
        let done = self.new_block();
        let blocks: Vec<BlockId> = discs.iter().map(|_| self.new_block()).collect();
        self.terminate(Terminator::Switch {
            value: Operand::Copy(Place::local(idx)),
            cases: blocks
                .iter()
                .enumerate()
                .map(|(i, b)| (i as i128, *b))
                .collect(),
            default: done,
        });
        for (d, b) in discs.iter().zip(blocks) {
            self.switch_to(b);
            self.assign(Place::local(out), Rvalue::Use(cint(*d as i128, Ty::I64)));
            self.goto(done);
        }
        self.switch_to(done);
        Operand::Copy(Place::local(out))
    }

    /// Read one JSON scalar and find the index of the matching alternative (a `U32` local);
    /// fail with the choice text if there is none.
    pub(super) fn json_match_choice(
        &mut self,
        r: Local,
        ctx: Local,
        alts: &[LitValue],
        fail: BlockId,
    ) -> Local {
        let expected = choice_text(alts);
        let idx = self.temp(Ty::U32);
        let (done, bad) = (self.new_block(), self.new_block());
        let strs = by_kind(alts, |l| matches!(l, LitValue::Str(_)));
        let nums = by_kind(alts, |l| {
            matches!(l, LitValue::Int(..) | LitValue::Float(..))
        });
        let bools = by_kind(alts, |l| matches!(l, LitValue::Bool(_)));
        let (sb, nb, bb) = (self.new_block(), self.new_block(), self.new_block());
        let mut cases = vec![];
        if !strs.is_empty() {
            cases.push((TOKEN_STRING as i128, sb));
        }
        if !nums.is_empty() {
            cases.push((TOKEN_NUMBER as i128, nb));
        }
        if !bools.is_empty() {
            cases.push((TOKEN_TRUE as i128, bb));
            cases.push((TOKEN_FALSE as i128, bb));
        }
        let ro = Operand::Copy(Place::local(r));
        let tok = self.temp(Ty::U32);
        self.call_rt(Rt::JsonPeek, vec![ro.clone()], Some(Place::local(tok)));
        self.terminate(Terminator::Switch {
            value: Operand::Copy(Place::local(tok)),
            cases,
            default: bad,
        });
        let cx = ChoiceCx {
            r,
            ctx,
            idx,
            expected: &expected,
            done,
            fail,
        };
        for (block, alts, kind) in [
            (sb, &strs, Scalar::Str),
            (nb, &nums, Scalar::Num),
            (bb, &bools, Scalar::Bool),
        ] {
            self.switch_to(block);
            match kind {
                _ if alts.is_empty() => self.goto(bad),
                Scalar::Str => self.match_str_choices(&cx, alts),
                Scalar::Num => self.match_num_choices(&cx, alts),
                Scalar::Bool => self.match_bool_choices(&cx, alts),
            }
        }
        // Another kind of value: skip it so a malformed one reports its syntax error.
        self.switch_to(bad);
        self.rt_u8(Rt::JsonSkipValue, vec![ro]);
        self.json_fail(ctx, &expected, fail);
        self.switch_to(done);
        idx
    }

    /// Strings: compare the decoded text with each string alternative.
    fn match_str_choices(&mut self, cx: &ChoiceCx<'_>, strs: &[(u32, LitValue)]) {
        let s = self.temp(STR);
        let sa = self.addr(Place::local(s));
        let ro = Operand::Copy(Place::local(cx.r));
        self.json_expect(
            Rt::JsonReadString,
            vec![ro, sa.clone()],
            cx.ctx,
            cx.expected,
            cx.fail,
        );
        for (i, l) in strs {
            let LitValue::Str(text) = l else {
                unreachable!("ICE: non-string literal among the string alternatives")
            };
            let lit = self.str_lit(text);
            let la = self.operand_addr(lit, STR);
            let eq = self.rt_u8(Rt::StrEq, vec![sa.clone(), la]);
            self.choice_hit(cx, eq, *i, Some(&sa));
        }
        self.call_rt(Rt::StrDrop, vec![sa], None);
        self.json_fail(cx.ctx, cx.expected, cx.fail);
    }

    /// Numbers: compare as f64 (literal and enum values are exact there).
    fn match_num_choices(&mut self, cx: &ChoiceCx<'_>, nums: &[(u32, LitValue)]) {
        let f = self.temp(Ty::F64);
        let fa = self.addr(Place::local(f));
        let ro = Operand::Copy(Place::local(cx.r));
        self.json_expect(Rt::JsonReadF64, vec![ro, fa], cx.ctx, cx.expected, cx.fail);
        for (i, l) in nums {
            let v = match l {
                LitValue::Int(_, n) => *n as f64,
                LitValue::Float(_, bits) => f64::from_bits(*bits),
                _ => unreachable!("ICE: non-number literal among the number alternatives"),
            };
            let c = Operand::Const(Const::Float(v), Ty::F64);
            let eq = self.rvalue_temp(
                Ty::Bool,
                Rvalue::Binary(BinOp::Eq, Operand::Copy(Place::local(f)), c),
            );
            self.choice_hit(cx, eq, *i, None);
        }
        self.json_fail(cx.ctx, cx.expected, cx.fail);
    }

    /// Bools.
    fn match_bool_choices(&mut self, cx: &ChoiceCx<'_>, bools: &[(u32, LitValue)]) {
        let b = self.temp(Ty::U8);
        let ba = self.addr(Place::local(b));
        let ro = Operand::Copy(Place::local(cx.r));
        self.json_expect(Rt::JsonReadBool, vec![ro, ba], cx.ctx, cx.expected, cx.fail);
        for (i, l) in bools {
            let LitValue::Bool(want) = l else {
                unreachable!("ICE: non-bool literal among the bool alternatives")
            };
            let eq = self.rvalue_temp(
                Ty::Bool,
                Rvalue::Binary(
                    BinOp::Eq,
                    Operand::Copy(Place::local(b)),
                    cint(*want as i128, Ty::U8),
                ),
            );
            self.choice_hit(cx, eq, *i, None);
        }
        self.json_fail(cx.ctx, cx.expected, cx.fail);
    }

    /// If `eq`, alternative `i` matched: drop the decoded string `drop` (if any), store `i`
    /// and continue at `done`; otherwise go on with the next comparison.
    fn choice_hit(&mut self, cx: &ChoiceCx<'_>, eq: Operand, i: u32, drop: Option<&Operand>) {
        let (hit, miss) = (self.new_block(), self.new_block());
        self.branch(eq, hit, miss);
        self.switch_to(hit);
        if let Some(sa) = drop {
            self.call_rt(Rt::StrDrop, vec![sa.clone()], None);
        }
        self.assign(Place::local(cx.idx), Rvalue::Use(cint(i as i128, Ty::U32)));
        self.goto(cx.done);
        self.switch_to(miss);
    }
}

/// What the comparisons of [`FnLower::json_match_choice`] need.
struct ChoiceCx<'a> {
    r: Local,
    ctx: Local,
    /// The matching alternative's index goes here.
    idx: Local,
    expected: &'a str,
    /// Matched: continue here.
    done: BlockId,
    fail: BlockId,
}

/// The kinds of token a choice can be read from.
enum Scalar {
    Str,
    Num,
    Bool,
}

/// The alternatives `want` accepts, with their indexes.
fn by_kind(alts: &[LitValue], want: fn(&LitValue) -> bool) -> Vec<(u32, LitValue)> {
    alts.iter()
        .enumerate()
        .filter(|(_, l)| want(l))
        .map(|(i, l)| (i as u32, l.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use velt_sema::hir::IntTy;

    #[test]
    fn choices_are_listed_by_kind_whatever_the_member_order() {
        let alts = [
            LitValue::Bool(false),
            LitValue::Str("auto".into()),
            LitValue::Int(IntTy::I64, 0),
            LitValue::Float(hir::FloatTy::F64, 1.5f64.to_bits()),
        ];
        assert_eq!(choice_text(&alts), r#"one of "auto", 0, 1.5, false"#);
        let enum_like = [LitValue::Int(IntTy::I64, 20), LitValue::Int(IntTy::I64, 10)];
        assert_eq!(choice_text(&enum_like), "one of 20, 10");
        assert_eq!(choice_text(&[LitValue::Str("x".into())]), r#""x""#);
    }
}
