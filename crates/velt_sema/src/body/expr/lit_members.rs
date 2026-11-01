//! The members of a union a literal belongs to: the one it names (`lit_member`), or, against a
//! union with several number members, each that holds its value.
//!
//! A number literal against a union with several number members (`x === 1`, `case 1:` with
//! `x: f64 | u8`, #1890). Every number is a number in TypeScript, so the literal equals each
//! member whose type holds its value exactly: `1` tests the `f64` member and the `u8` one,
//! `1.5` only the `f64` one, `300` only the `f64` one. A literal-type member (`1` in
//! `1 | 2 | u8`) is a number member too, holding its one value. When no member holds the value
//! exactly, the float members take it rounded, as a plain float type does (`0.1` against
//! `f32 | u8`, `2 ** 53 + 1` against `f64 | u8`). Equality of two such union values is the
//! lowering's (`velt_vir` `glue/eq_numbers.rs`); narrowing keeps every member tested.

use velt_common::Span;
use velt_syntax::ast;

use crate::body::FnCx;
use crate::hir::{self, FloatTy, LitValue, PatKind as P, TyId, TyKind};

/// The value of a number literal: an integer, or a float that is not one.
#[derive(Clone, Copy)]
enum Num {
    Int(i128),
    Float(f64),
}

/// A float value as a [`Num`]: a whole number other than zero is an integer.
fn float_num(f: f64) -> Num {
    if f.fract() == 0.0 && f.abs() < 1e38 && f != 0.0 {
        Num::Int(f as i128)
    } else {
        Num::Float(f)
    }
}

/// Two numbers are the same value (`0` and `-0` alike, as in `===`).
fn same_num(a: Num, b: Num) -> bool {
    match (a, b) {
        (Num::Int(a), Num::Int(b)) => a == b,
        (Num::Float(a), Num::Float(b)) => a == b,
        (Num::Int(i), Num::Float(f)) | (Num::Float(f), Num::Int(i)) => i == 0 && f == 0.0,
    }
}

impl FnCx<'_, '_> {
    /// The member of union `u` a literal pattern / operand belongs to, or the note of the
    /// mismatch (naming `u`).
    pub(crate) fn lit_member(
        &mut self,
        u: TyId,
        lit: &ast::SignedLit,
    ) -> Result<(u32, TyId), String> {
        if self
            .number_lit_members(u, lit)
            .is_some_and(|f| f.is_empty())
        {
            let un = self.cx.display(u);
            return Err(format!("no number member of `{un}` holds this value"));
        }
        let members = self.cx.union_members(u).unwrap_or_default();
        // A literal-type member with this value wins over its base type.
        let exact = members.iter().position(|m| {
            self.cx
                .lit_value(*m)
                .is_some_and(|v| crate::literals::lit_matches(&v, lit))
        });
        if let Some(i) = exact {
            return Ok((i as u32, members[i]));
        }
        let ty = &self.cx.ty;
        let find = |f: &dyn Fn(TyId) -> bool| -> Vec<usize> {
            (0..members.len()).filter(|&i| f(members[i])).collect()
        };
        let (found, what) = match &lit.lit {
            ast::Lit::Str(_) => (find(&|t| t == ty.str_), "string"),
            ast::Lit::Bool(_) => (find(&|t| t == ty.bool_), "boolean"),
            ast::Lit::Int {
                suffix: Some(s), ..
            }
            | ast::Lit::Float {
                suffix: Some(s), ..
            } => {
                let st = self.cx.ty.primitive(s);
                (find(&|t| Some(t) == st), "number")
            }
            ast::Lit::Int { .. } => {
                let ints = find(&|t| ty.is_int(t));
                if ints.is_empty() {
                    (find(&|t| ty.is_float(t)), "number")
                } else {
                    (ints, "integer")
                }
            }
            ast::Lit::Float { .. } => (find(&|t| ty.is_float(t)), "float"),
            ast::Lit::Null => (vec![], "null"),
            ast::Lit::Undefined => (vec![], "undefined"),
        };
        if let [i] = found[..] {
            return Ok((i as u32, members[i]));
        }
        let un = self.cx.display(u);
        match found.as_slice() {
            [] => Err(format!("this {what} literal is not a member of `{un}`")),
            _ => Err(format!(
                "this {what} literal could be several members of `{un}` (add a type suffix like \
                 `5i32`)"
            )),
        }
    }

    /// [`lit_member`](Self::lit_member) without diagnostics.
    pub(crate) fn lit_member_quiet(&mut self, u: TyId, lit: ast::SignedLit) -> Option<(u32, TyId)> {
        self.lit_member(u, &lit).ok()
    }

    /// The number members of union `u` that number literal `lit` equals, each with the literal
    /// as a value of that member's type (`None` for a literal-type member, which is that one
    /// value). When none holds the value exactly, the float members, with the literal rounded to
    /// their type. `None` when `u` has fewer than two number members, or only literal-type
    /// ones, or `lit` is not a number: the one-member rules apply (`lit_member`).
    pub(crate) fn number_lit_members(
        &mut self,
        u: TyId,
        lit: &ast::SignedLit,
    ) -> Option<Vec<(u32, TyId, Option<hir::Lit>)>> {
        let members = self.cx.union_members(u)?;
        let numbers: Vec<(u32, TyId)> = (0..members.len() as u32)
            .map(|i| (i, members[i as usize]))
            .filter(|&(_, m)| self.cx.ty.is_numeric(m) || self.lit_num(m).is_some())
            .collect();
        // A union of literal types only (`1 | 2 | 3`) holds each value once.
        let plain = numbers.iter().any(|&(_, m)| self.cx.ty.is_numeric(m));
        if numbers.len() < 2 || !plain {
            return None;
        }
        let n = self.lit_number(lit)?;
        let mut found = vec![];
        for &(v, m) in &numbers {
            match self.lit_num(m) {
                Some(c) if same_num(c, n) => found.push((v, m, None)),
                Some(_) => {}
                None => {
                    if let Some(l) = self.number_as(n, m) {
                        found.push((v, m, Some(l)));
                    }
                }
            }
        }
        if found.is_empty() {
            for &(v, m) in &numbers {
                if let Some(l) = self.number_rounded(n, m) {
                    found.push((v, m, Some(l)));
                }
            }
        }
        Some(found)
    }

    /// The pattern of union `u` that number literal `lit` matches (an or-pattern over the number
    /// members it equals) and those members, when [`number_lit_members`](Self::number_lit_members)
    /// applies and finds one.
    pub(crate) fn number_lit_pat(
        &mut self,
        u: TyId,
        lit: &ast::SignedLit,
        lit_span: Span,
        span: Span,
    ) -> Option<(hir::Pat, Vec<u32>)> {
        let found = self.number_lit_members(u, lit)?;
        let (def, _) = self.cx.union_def(u)?;
        let mut pats = vec![];
        let mut variants = vec![];
        for (variant, m, l) in found {
            let lp = match l {
                Some(l) => self.pat(P::Lit(l), m, lit_span),
                // A literal-type member is that one value.
                None => self.pat(P::Wildcard, m, lit_span),
            };
            let args = vec![lp];
            pats.push(self.pat(P::Variant { def, variant, args }, u, span));
            variants.push(variant);
        }
        let p = match pats.len() {
            0 => return None,
            1 => pats.pop()?,
            _ => self.pat(P::Or(pats), u, span),
        };
        Some((p, variants))
    }

    /// The exact value of a number literal (a suffix gives its type: `0.1f32` is the `f32`
    /// nearest 0.1).
    fn lit_number(&mut self, lit: &ast::SignedLit) -> Option<Num> {
        let (f, suffix) = match &lit.lit {
            ast::Lit::Int { value, suffix } => {
                let n = i128::try_from(*value).ok()?;
                let n = if lit.negative { -n } else { n };
                let float = suffix
                    .as_deref()
                    .and_then(|s| self.cx.ty.primitive(s))
                    .is_some_and(|t| self.cx.ty.is_float(t));
                if !float {
                    return Some(Num::Int(n));
                }
                (n as f64, suffix)
            }
            ast::Lit::Float { value, suffix } => {
                (if lit.negative { -value } else { *value }, suffix)
            }
            _ => return None,
        };
        let f = if suffix.as_deref() == Some("f32") {
            f as f32 as f64
        } else {
            f
        };
        Some(float_num(f))
    }

    /// The value of numeric literal type `m` (`1` in `1 | u8`).
    fn lit_num(&self, m: TyId) -> Option<Num> {
        match self.cx.lit_value(m)? {
            LitValue::Int(_, n) => Some(Num::Int(n)),
            LitValue::Float(ft, bits) => {
                let f = f64::from_bits(bits);
                Some(float_num(if ft == FloatTy::F32 {
                    f as f32 as f64
                } else {
                    f
                }))
            }
            _ => None,
        }
    }

    /// `n` rounded to float type `m`, as a plain float type takes a literal it doesn't hold
    /// exactly (`0.1` as an `f32`); `None` for other types and for a value out of `m`'s range.
    fn number_rounded(&mut self, n: Num, m: TyId) -> Option<hir::Lit> {
        let TyKind::Float(ft) = self.cx.ty.kind(m) else {
            return None;
        };
        let f = match n {
            Num::Int(n) => n as f64,
            Num::Float(f) => f,
        };
        let r = if *ft == FloatTy::F32 {
            f as f32 as f64
        } else {
            f
        };
        (r.is_finite() || !f.is_finite()).then_some(hir::Lit::Float(r))
    }

    /// `n` as a literal of number type `m`, when `m` holds it exactly.
    fn number_as(&mut self, n: Num, m: TyId) -> Option<hir::Lit> {
        match self.cx.ty.kind(m) {
            TyKind::Int(it) => {
                let it = *it;
                let Num::Int(n) = n else {
                    // `-0` and `0.0` are the integer `0`.
                    return matches!(n, Num::Float(f) if f == 0.0).then_some(hir::Lit::Int(0));
                };
                let bits = it.bits();
                let (lo, hi) = if it.is_signed() {
                    (-(1i128 << (bits - 1)), (1i128 << (bits - 1)) - 1)
                } else {
                    (0, (1i128 << bits) - 1)
                };
                let mask = (1u128 << bits) - 1;
                (lo..=hi)
                    .contains(&n)
                    .then_some(hir::Lit::Int(n as u128 & mask))
            }
            TyKind::Float(ft) => {
                let ft = *ft;
                let f = match n {
                    Num::Int(n) => (n as f64 as i128 == n).then_some(n as f64)?,
                    Num::Float(f) => f,
                };
                let fits = ft == FloatTy::F64 || f as f32 as f64 == f;
                fits.then_some(hir::Lit::Float(f))
            }
            _ => None,
        }
    }
}
