//! A string `+` with a union that has an array or tuple member (`"x" + v` with
//! `v: string | number[]`): the union is written as `String(v)` writes it, by the member it
//! holds (lowering writes an array member as `Array.prototype.toString` does, #1274).

use crate::body::FnCx;
use crate::hir::{TyId, TyKind};

impl FnCx<'_, '_> {
    /// Is `t` a union with an array or tuple member (also nullable)? Not for a recursive union
    /// (`type Child = string | Child[]`), whose text lowering cannot write as JS does.
    pub(crate) fn union_text_differs(&mut self, t: TyId) -> bool {
        let Some(members) = self.cx.union_members(t) else {
            return false;
        };
        members.iter().any(|m| self.js_list_member(*m))
            && !members.iter().any(|m| self.reaches(*m, t, 0))
    }

    /// Is `t` an array or a tuple (or a nullable one)?
    fn js_list_member(&self, t: TyId) -> bool {
        match self.cx.ty.kind(t) {
            TyKind::Array(_) | TyKind::Tuple(_) => true,
            TyKind::Option(p, _) => self.js_list_member(*p),
            _ => false,
        }
    }

    /// Does `t` hold a `target` (through arrays, tuples, nullables and unions)? Deep types
    /// count as holding it.
    fn reaches(&mut self, t: TyId, target: TyId, depth: u32) -> bool {
        if t == target || depth > 8 {
            return true;
        }
        match self.cx.ty.kind(t).clone() {
            TyKind::Array(e) | TyKind::Option(e, _) => self.reaches(e, target, depth + 1),
            TyKind::Tuple(ts) => ts.into_iter().any(|e| self.reaches(e, target, depth + 1)),
            _ => match self.cx.union_members(t) {
                Some(ms) => ms.into_iter().any(|m| self.reaches(m, target, depth + 1)),
                None => false,
            },
        }
    }
}
