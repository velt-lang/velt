//! A boxed array that code changing the caller's containers returns in a `read` callback
//! (`splice`'s removed elements, as a `T[]` the program also keeps in a `Map<string, T[]>`):
//! handed to the reader element by element (caller_stores.rs `hand_to_reader`, rt_abi_async.md
//! §9.1), when the reader alone holds it. Read mode counts the array itself as any object
//! outside the lock set; the lock-set objects in it lose their reference by the counts outside
//! read mode and are borrowed uncounted from then on, so the reader's drop of the array lets
//! go of none of them.

use velt_sema::hir::{self, ExprKind as K, Intrinsic, PassMode, TyId, TyKind};

use super::operand::proj;
use super::FnLower;
use crate::vir::{BinOp, Operand, Place, Proj, Rvalue, Ty};

impl FnLower<'_, '_> {
    /// Can the result of type `ty` of a call changing the caller's containers, given `args`
    /// passed by `modes`, be handed to the reader (`hand_to_reader`): none to hand, `hand_ok`,
    /// or a boxed array of what is (module docs) when the call is given nothing it could put
    /// in it uncounted, only the caller's places, what it keeps (owned, `keep_for_caller`) and
    /// values outside the lock set (`out.splice(i, n)`, not `all.concat(s.items)`)?
    pub(super) fn hand_result_ok(
        &mut self,
        ty: TyId,
        args: &[hir::Expr],
        modes: &[PassMode],
    ) -> bool {
        if !self.needs_drop(ty) || self.hand_ok(ty) {
            return true;
        }
        let TyKind::Array(e) = self.cx.kind(ty) else {
            return false;
        };
        if !self.cx.boxed(ty) || self.cx.ty(ty) != Ty::Ptr || self.cx.lock_counted(ty) {
            return false;
        }
        self.hand_ok(e)
            && args
                .iter()
                .zip(modes)
                .all(|(a, m)| *m == PassMode::Owned || self.lent_fresh(a))
    }

    /// Is the argument `a` outside the lock set, the caller's place, or made here of literals
    /// and what sema transferred (a rest parameter's array literal, call.rs `lent_copy`)?
    fn lent_fresh(&mut self, a: &hir::Expr) -> bool {
        let t = self.sub(a.ty);
        if !self.cx.reaches_lock_set(t) || self.captured_root(a) {
            return true;
        }
        match &a.kind {
            K::Lit(_) => true,
            K::ArrayLit(xs) => xs.iter().all(|x| self.lent_fresh(x)),
            K::Call {
                callee: hir::Callee::Intrinsic(Intrinsic::Transfer),
                ..
            } => true,
            _ => false,
        }
    }

    /// `hand_to_reader` of the value at `p` of type `ty` when it is a boxed array outside the
    /// lock set (`hand_result_ok`): one referenced once has each element handed; one held
    /// elsewhere too keeps its elements counted there.
    pub(super) fn hand_boxed(&mut self, p: &Place, ty: TyId) {
        let TyKind::Array(e) = self.cx.kind(ty) else {
            return;
        };
        if !self.cx.boxed(ty) || self.cx.ty(ty) != Ty::Ptr || self.cx.lock_counted(ty) {
            return;
        }
        let o = self.rvalue_temp(Ty::Ptr, Rvalue::Use(Operand::Copy(p.clone())));
        let nn = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Ne, o.clone(), super::cint(0, Ty::Ptr)),
        );
        let (obj, walk, done) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(nn, obj, done);
        self.switch_to(obj);
        let c = self.count_place(o.clone());
        let one = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Eq, Operand::Copy(c), super::cint(1, Ty::U64)),
        );
        self.branch(one, walk, done);
        self.switch_to(walk);
        let op = Place::local(self.copy_to_temp(o, Ty::Ptr));
        let arr = self.content(&op, ty);
        let k = self.temp(Ty::U64);
        self.assign(Place::local(k), Rvalue::Use(super::cint(0, Ty::U64)));
        let len = Operand::Copy(proj(&arr, Proj::Field(1)));
        self.count_loop(k, len, |lw, k| {
            let ep = lw.elem_place(&arr, k, e);
            lw.hand_to_reader(&Operand::Copy(ep), e);
        });
        self.goto(done);
        self.switch_to(done);
    }
}
