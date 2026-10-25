//! `Array.isArray(x)` on a union with a type parameter member (`x: T | string`), for
//! [`super::narrow`]'s `is_array_facts`.

use super::narrow::Fact;
use super::FnCx;
use crate::hir::{LocalId, TyId, TyKind};

impl FnCx<'_, '_> {
    /// Where the test holds, the value sits in a type parameter member (TypeScript's
    /// `T & any[]`, `T` may be an array), read as an `any` as a variable of a type parameter
    /// is; where it fails, every member stays (`T` may be any type). `None` for a union without
    /// such a member.
    pub(super) fn generic_member_array_facts(
        &mut self,
        l: LocalId,
        nullable: bool,
        u: TyId,
    ) -> Option<(Vec<Fact>, Vec<Fact>)> {
        let members = self.cx.union_members(u)?;
        let params: Vec<u32> = (0..members.len() as u32)
            .filter(|&i| matches!(self.cx.ty.kind(members[i as usize]), TyKind::Param(_)))
            .collect();
        let any = self.any_ty().filter(|_| !params.is_empty())?;
        let pred = crate::body::expr::object_builtins::is_array_ty;
        let (mut t, f) = self.split_facts(l, nullable, u, &pred, false);
        for fact in &mut t {
            if let Fact::Members(_, yes) = fact {
                yes.extend(params.iter().copied());
                yes.sort_unstable();
            }
        }
        if let [Fact::Members(_, yes), ..] = t.as_slice() {
            if yes.len() == params.len() {
                t.push(Fact::Class(l, any));
            }
        }
        let f = f
            .into_iter()
            .filter(|x| !matches!(x, Fact::Members(..)))
            .collect();
        Some((t, f))
    }
}
