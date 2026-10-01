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

/// `"low"` for one choice, `one of "low", "high"` for several.
fn choice_text(alts: &[LitValue]) -> String {
    let texts: Vec<String> = alts.iter().map(literal_text).collect();
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
        let ro = Operand::Copy(Place::local(r));
        let (done, bad) = (self.new_block(), self.new_block());
        let by_kind = |want: fn(&LitValue) -> bool| -> Vec<(u32, LitValue)> {
            alts.iter()
                .enumerate()
                .filter(|(_, l)| want(l))
                .map(|(i, l)| (i as u32, l.clone()))
                .collect()
        };
        let strs = by_kind(|l| matches!(l, LitValue::Str(_)));
        let nums = by_kind(|l| matches!(l, LitValue::Int(..) | LitValue::Float(..)));
        let bools = by_kind(|l| matches!(l, LitValue::Bool(_)));
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
        let tok = self.temp(Ty::U32);
        self.call_rt(Rt::JsonPeek, vec![ro.clone()], Some(Place::local(tok)));
        self.terminate(Terminator::Switch {
            value: Operand::Copy(Place::local(tok)),
            cases,
            default: bad,
        });

        // Strings: compare the decoded text with each string alternative.
        self.switch_to(sb);
        if strs.is_empty() {
            self.goto(bad);
        } else {
            let s = self.temp(STR);
            let sa = self.addr(Place::local(s));
            self.json_expect(Rt::JsonReadString, vec![ro.clone(), sa.clone()], ctx, &expected, fail);
            for (i, l) in &strs {
                let LitValue::Str(text) = l else { unreachable!() };
                let lit = self.str_lit(text);
                let la = self.operand_addr(lit, STR);
                let eq = self.rt_u8(Rt::StrEq, vec![sa.clone(), la]);
                let (hit, miss) = (self.new_block(), self.new_block());
                self.branch(eq, hit, miss);
                self.switch_to(hit);
                self.call_rt(Rt::StrDrop, vec![sa.clone()], None);
                self.assign(Place::local(idx), Rvalue::Use(cint(*i as i128, Ty::U32)));
                self.goto(done);
                self.switch_to(miss);
            }
            self.call_rt(Rt::StrDrop, vec![sa], None);
            self.json_fail(ctx, &expected, fail);
        }

        // Numbers: compare as f64 (literal and enum values are exact there).
        self.switch_to(nb);
        if nums.is_empty() {
            self.goto(bad);
        } else {
            let f = self.temp(Ty::F64);
            let fa = self.addr(Place::local(f));
            self.json_expect(Rt::JsonReadF64, vec![ro.clone(), fa], ctx, &expected, fail);
            for (i, l) in &nums {
                let v = match l {
                    LitValue::Int(_, n) => *n as f64,
                    LitValue::Float(_, bits) => f64::from_bits(*bits),
                    _ => unreachable!(),
                };
                let c = Operand::Const(Const::Float(v), Ty::F64);
                let eq = self.rvalue_temp(
                    Ty::Bool,
                    Rvalue::Binary(BinOp::Eq, Operand::Copy(Place::local(f)), c),
                );
                let (hit, miss) = (self.new_block(), self.new_block());
                self.branch(eq, hit, miss);
                self.switch_to(hit);
                self.assign(Place::local(idx), Rvalue::Use(cint(*i as i128, Ty::U32)));
                self.goto(done);
                self.switch_to(miss);
            }
            self.json_fail(ctx, &expected, fail);
        }

        // Bools.
        self.switch_to(bb);
        if bools.is_empty() {
            self.goto(bad);
        } else {
            let b = self.temp(Ty::U8);
            let ba = self.addr(Place::local(b));
            self.json_expect(Rt::JsonReadBool, vec![ro.clone(), ba], ctx, &expected, fail);
            for (i, l) in &bools {
                let LitValue::Bool(want) = l else { unreachable!() };
                let eq = self.rvalue_temp(
                    Ty::Bool,
                    Rvalue::Binary(
                        BinOp::Eq,
                        Operand::Copy(Place::local(b)),
                        cint(*want as i128, Ty::U8),
                    ),
                );
                let (hit, miss) = (self.new_block(), self.new_block());
                self.branch(eq, hit, miss);
                self.switch_to(hit);
                self.assign(Place::local(idx), Rvalue::Use(cint(*i as i128, Ty::U32)));
                self.goto(done);
                self.switch_to(miss);
            }
            self.json_fail(ctx, &expected, fail);
        }

        // Another kind of value: skip it so a malformed one reports its syntax error.
        self.switch_to(bad);
        self.rt_u8(Rt::JsonSkipValue, vec![ro]);
        self.json_fail(ctx, &expected, fail);

        self.switch_to(done);
        idx
    }
}
