//! A union value tested for truthiness whose members include falsy literal types (`"" | "a"`,
//! `0 | 1`, `false | "x"`), as in TypeScript (#3370):
//! - `if (s)`, `s ? … : …` on a union local or a field path of one (`o.s`): when true it is none
//!   of those members; when false it is one of the members that can be falsy (the falsy
//!   literals and the primitive types: `string` may be `""`);
//! - the value of `a || b` / `a && b` (any `a`, also a chain `s || t || "c"`): the left side's
//!   value is taken only when it is truthy / falsy, so it is narrowed to those members.

use velt_syntax::ast;

use super::narrow::Fact;
use super::FnCx;
use crate::hir::{self, LitValue, TyId, TyKind};

impl FnCx<'_, '_> {
    /// The facts of `e` being truthy / falsy when it is a union local, or a field path of one,
    /// with a falsy literal member: the members it can then be. `None` for anything else.
    pub(super) fn falsy_member_facts(&mut self, e: &ast::Expr) -> Option<(Vec<Fact>, Vec<Fact>)> {
        let (l, nullable, u) = match &e.kind {
            ast::ExprKind::Ident(_) => self.union_local(e)?,
            ast::ExprKind::Member {
                optional: false, ..
            } => {
                let (l, nullable, u, _) = self.field_with_members(e)?;
                (l, nullable, u)
            }
            _ => return None,
        };
        self.cx.union_def(u)?;
        let truthy = self.truthy_members(u, true)?;
        let t = vec![Fact::Members(l, truthy)];
        // A falsy `T | null` may be the `null`, which a members fact can't say.
        let f = match nullable {
            true => vec![],
            false => self
                .truthy_members(u, false)
                .map(|vs| vec![Fact::Members(l, vs)])
                .unwrap_or_default(),
        };
        Some((t, f))
    }

    /// The members of union `u` a truthy (`truthy`) or falsy value of it can be, when `u` has a
    /// falsy literal member and that rules out some (`None` otherwise).
    pub(super) fn truthy_members(&mut self, u: TyId, truthy: bool) -> Option<Vec<u32>> {
        let members = self.cx.union_members(u)?;
        if !members.iter().any(|m| self.falsy_literal(*m)) {
            return None;
        }
        let keep: Vec<u32> = (0..members.len() as u32)
            .filter(|&v| {
                let m = members[v as usize];
                match truthy {
                    true => !self.falsy_literal(m),
                    false => self.can_be_falsy(m),
                }
            })
            .collect();
        (keep.len() < members.len()).then_some(keep)
    }

    /// `h`, a union value known to be truthy (`truthy`) or falsy, as the union of the members
    /// it can then be (one member: that member's type). `h` itself when nothing is ruled out.
    pub(super) fn narrow_operand(&mut self, h: hir::Expr, truthy: bool) -> hir::Expr {
        let Some(keep) = self.truthy_members(h.ty, truthy) else {
            return h;
        };
        let members = self.cx.union_members(h.ty).unwrap_or_default();
        let kept: Vec<TyId> = keep.iter().map(|v| members[*v as usize]).collect();
        let target = match kept.as_slice() {
            [one] => *one,
            _ => self.cx.union_of(&kept, false, h.span),
        };
        let back = h.clone();
        self.map_members_in(h, target, Some(keep), &mut |s, value| {
            s.try_coerce(value, target).ok()
        })
        .unwrap_or(back)
    }

    /// Is `t` a literal type of a falsy value (`""`, `0`, `-0`, `NaN`, `false`)?
    fn falsy_literal(&self, t: TyId) -> bool {
        match self.cx.lit_value(t) {
            Some(LitValue::Str(s)) => s.is_empty(),
            Some(LitValue::Int(_, n)) => n == 0,
            Some(LitValue::Float(_, bits)) => {
                let f = f64::from_bits(bits);
                f == 0.0 || f.is_nan()
            }
            Some(LitValue::Bool(b)) => !b,
            None => false,
        }
    }

    /// Can a value of `t` be falsy: a falsy literal, or a primitive type (`string` may be `""`,
    /// a number `0`, a `boolean` `false`)? Objects, arrays and functions are always truthy.
    fn can_be_falsy(&self, t: TyId) -> bool {
        if self.cx.lit_value(t).is_some() {
            return self.falsy_literal(t);
        }
        let ty = &self.cx.ty;
        t == ty.str_
            || t == ty.bool_
            || ty.is_numeric(t)
            || matches!(ty.kind(t), TyKind::Option(..))
    }
}
