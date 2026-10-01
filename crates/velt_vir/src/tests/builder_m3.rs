//! Test-only helpers for hand-building M3/M4 HIR: async functions, `await`, promise types and
//! the async/JSON intrinsics, shaped like sema's output (hir.rs "M3 additions").

use velt_sema::hir::*;

use super::builder::{ex, intr, FB, PB};

impl PB {
    pub fn promise(&mut self, t: TyId) -> TyId {
        let never = self.t.never;
        self.ty(TyKind::Promise(t, never))
    }
    pub fn shared(&mut self, t: TyId) -> TyId {
        self.ty(TyKind::Shared(t))
    }
}

impl FB {
    /// An `async function` (its `ret` is `Promise<T>`).
    pub fn build_async(self, stmts: Vec<Stmt>) -> FnDef {
        let mut f = self.build(stmts);
        f.is_async = true;
        f
    }
}

pub(super) fn await_(e: Expr, ty: TyId) -> Expr {
    ex(ExprKind::Await(Box::new(e)), ty)
}

pub(super) fn sleep(ms: Expr, promise_void: TyId) -> Expr {
    intr(Intrinsic::Sleep, vec![ms], promise_void)
}

pub(super) fn spawn(p: Expr, ty: TyId) -> Expr {
    intr(Intrinsic::Spawn, vec![p], ty)
}

pub(super) fn promise_all(ps: Expr, ty: TyId) -> Expr {
    intr(Intrinsic::PromiseAll, vec![ps], ty)
}
