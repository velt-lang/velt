//! A union local tested for truthiness (`if (s)`, `s ? s : "c"`, `s || "c"`) whose members
//! include falsy literal types (`"" | "a"`, `0 | 1`, `false | "x"`): when true it is none of
//! those members, as in TypeScript (#3370).

use velt_syntax::ast;

use super::narrow::Fact;
use super::FnCx;
use crate::hir::{LitValue, TyId};

impl FnCx<'_, '_> {
    /// The facts of `e` being truthy when it names a union local with a falsy literal member:
    /// the local is one of the other members. `None` for anything else.
    pub(super) fn falsy_member_facts(&mut self, e: &ast::Expr) -> Option<Vec<Fact>> {
        if !matches!(e.kind, ast::ExprKind::Ident(_)) {
            return None;
        }
        let (l, _, u) = self.union_local(e)?;
        let members = self.cx.union_members(u)?;
        let kept: Vec<u32> = (0..members.len() as u32)
            .filter(|&v| !self.falsy_literal(members[v as usize]))
            .collect();
        (kept.len() < members.len()).then(|| vec![Fact::Members(l, kept)])
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
}
