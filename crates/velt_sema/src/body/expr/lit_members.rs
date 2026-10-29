//! The members of a union a literal belongs to: the one it names (`lit_member`), or, against a
//! union with several number members, each that holds its value.
//!
//! A number literal against a union with several number members (`x === 1`, `case 1:` with
//! `x: f64 | u8`, #1890). Every number is a number in TypeScript, so the literal equals each
//! member whose type holds its value exactly: `1` tests the `f64` member and the `u8` one,
//! `1.5` only the `f64` one, `300` only the `f64` one. Equality of two such union values is
//! the lowering's (`velt_vir` `glue/eq_numbers.rs`); narrowing keeps every member tested.

use velt_common::Span;
use velt_syntax::ast;

use crate::body::FnCx;
use crate::hir::{self, FloatTy, PatKind as P, TyId, TyKind};

/// The value of a number literal: an integer, or a float that is not one.
#[derive(Clone, Copy)]
enum Num {
    Int(i128),
    Float(f64),
}

impl FnCx<'_, '_> {
    /// The member of union `u` a literal pattern / operand belongs to.
    pub(crate) fn lit_member(
        &mut self,
        u: TyId,
        lit: &ast::SignedLit,
    ) -> Result<(u32, TyId), String> {
        if self
            .number_lit_members(u, lit)
            .is_some_and(|f| f.is_empty())
        {
            return Err("this number literal is out of range for every number member".into());
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
        match found.as_slice() {
            [i] => Ok((*i as u32, members[*i])),
            [] => Err(format!("this {what} literal is not a member")),
            _ => Err(format!(
                "this {what} literal could be several members (add a type suffix like `5i32`)"
            )),
        }
    }

    /// [`lit_member`](Self::lit_member) without diagnostics.
    pub(crate) fn lit_member_quiet(&mut self, u: TyId, lit: ast::SignedLit) -> Option<(u32, TyId)> {
        self.lit_member(u, &lit).ok()
    }

    /// The number members of union `u` that number literal `lit` equals, each with the literal
    /// as a value of that member's type. `None` when `u` has fewer than two number members, or
    /// `lit` is not a number or names a literal-type member (`1 | f64`): the one-member rules
    /// apply (`lit_member`).
    pub(crate) fn number_lit_members(
        &mut self,
        u: TyId,
        lit: &ast::SignedLit,
    ) -> Option<Vec<(u32, TyId, hir::Lit)>> {
        let members = self.cx.union_members(u)?;
        let exact = members.iter().any(|&m| {
            self.cx
                .lit_value(m)
                .is_some_and(|v| crate::literals::lit_matches(&v, lit))
        });
        let numbers: Vec<(u32, TyId)> = (0..members.len() as u32)
            .map(|i| (i, members[i as usize]))
            .filter(|&(_, m)| self.cx.ty.is_numeric(m) && self.cx.lit_value(m).is_none())
            .collect();
        if exact || numbers.len() < 2 {
            return None;
        }
        let n = self.lit_number(lit)?;
        Some(
            numbers
                .into_iter()
                .filter_map(|(v, m)| Some((v, m, self.number_as(n, m)?)))
                .collect(),
        )
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
            let lp = self.pat(P::Lit(l), m, lit_span);
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
        Some(if f.fract() == 0.0 && f.abs() < 1e38 && f != 0.0 {
            Num::Int(f as i128)
        } else {
            Num::Float(f)
        })
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
