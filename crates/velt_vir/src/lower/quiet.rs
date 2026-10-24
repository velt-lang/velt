//! Quiet functions: a direct call to one needs no stabilized borrows (stabilize.rs), because
//! the callee cannot reach the counted objects a borrow goes through, nor replace or free
//! anything. That is what a runtime extern guarantees too (`Stabilize::No` in call.rs).
//!
//! A function is quiet when its body runs no user code and drops nothing it did not make: it
//! is not generic, a method, a closure, async or a generator; its statements are `let`s,
//! assignments, `if`, `while`, `return`, `break` and `continue`, and expressions; it only
//! assigns values that need no drop, to locals or through its params (elements and fields);
//! and it only calls quiet functions, `velt_simd_*` externs and intrinsics that run no user
//! code and are handed no place to modify or move. Anything else (`new`, closures, `await`,
//! `try`, `throw`, `for…of`, patterns, indirect, virtual and interface calls, module state)
//! makes it not quiet, and so does a field read or write on a type that may hold a typed proxy
//! (proxy_typed.rs): it may run a `get` or `set` trap, which is user code. Std's byte readers
//! and writers (`loadF64(this.buf.bytes, o)` in a typed array's element access) are quiet, so
//! the access counts no reference per element.

use velt_sema::hir::{self, DefId, ExprKind as H, StmtKind as S};

use super::Cx;

impl Cx<'_> {
    /// Is `def` a quiet function (module docs)? Memoized; a function reached again while it is
    /// being decided (recursion) counts as not quiet.
    pub(super) fn quiet(&mut self, def: DefId) -> bool {
        if let Some(&q) = self.quiet.get(&def) {
            return q;
        }
        self.quiet.insert(def, false);
        let hir::Def::Fn(f) = self.hir.def(def) else {
            return false;
        };
        let q = f.generics == 0
            && !f.is_async
            && !f.is_generator
            && f.self_ty.is_none()
            && f.captures.is_empty()
            && self.quiet_block(&f.body.block);
        self.quiet.insert(def, q);
        q
    }

    fn quiet_block(&mut self, b: &hir::Block) -> bool {
        b.stmts.iter().all(|s| self.quiet_stmt(s))
            && b.value.as_deref().is_none_or(|v| self.quiet_expr(v))
    }

    fn quiet_stmt(&mut self, s: &hir::Stmt) -> bool {
        match &s.kind {
            S::Let { local: _, init } => init.as_ref().is_none_or(|e| self.quiet_value(e)),
            S::Expr(e) => self.quiet_expr(e),
            S::Return(e) => e.as_ref().is_none_or(|e| self.quiet_expr(e)),
            S::If { cond, then, els } => {
                self.quiet_expr(cond)
                    && self.quiet_block(then)
                    && els.as_ref().is_none_or(|b| self.quiet_block(b))
            }
            S::While {
                cond, body, step, ..
            } => {
                self.quiet_expr(cond)
                    && self.quiet_block(body)
                    && step.as_ref().is_none_or(|e| self.quiet_expr(e))
            }
            S::Break(_) | S::Continue(_) => true,
            S::Block(b) => self.quiet_block(b),
            S::LetPat { .. } | S::ForOf { .. } | S::Try { .. } => false,
        }
    }

    /// A quiet expression whose value needs no drop (stored in a local or a place).
    fn quiet_value(&mut self, e: &hir::Expr) -> bool {
        !self.needs_drop(e.ty) && self.quiet_expr(e)
    }

    fn quiet_expr(&mut self, e: &hir::Expr) -> bool {
        match &e.kind {
            H::Lit(_) | H::Local(..) => true,
            H::Unary { expr: x, .. } | H::Cast(x) => self.quiet_expr(x),
            H::Binary { lhs, rhs, .. } | H::Logical { lhs, rhs, .. } => {
                self.quiet_expr(lhs) && self.quiet_expr(rhs)
            }
            H::Assign { place, value } | H::CompoundAssign { place, value, .. } => {
                // The old value is dropped too: neither may need a drop.
                !self.needs_drop(place.ty) && self.quiet_place(place) && self.quiet_value(value)
            }
            H::If { cond, then, els } => {
                self.quiet_expr(cond) && self.quiet_expr(then) && self.quiet_expr(els)
            }
            H::Block(b) => self.quiet_block(b),
            H::Field { base, mode, .. } => {
                *mode != hir::UseMode::Move
                    && !self.needs_drop(e.ty)
                    && !self.proxiable(base.ty)
                    && self.quiet_expr(base)
            }
            H::Index { base, index, mode } => {
                *mode != hir::UseMode::Move
                    && !self.needs_drop(e.ty)
                    && self.quiet_expr(base)
                    && self.quiet_expr(index)
            }
            H::Call { callee, args } => {
                let callee_quiet = match callee {
                    hir::Callee::Def(def, targs) => match self.hir.def(*def) {
                        hir::Def::Fn(_) => targs.is_empty() && self.quiet(*def),
                        hir::Def::ExternFn(x) => !x.is_async && x.symbol.starts_with("velt_simd_"),
                        _ => false,
                    },
                    hir::Callee::Intrinsic(i) => {
                        pure_intrinsic(*i) && !args.iter().any(velt_sema::effects::writes_place)
                    }
                    _ => false,
                };
                callee_quiet && args.iter().all(|a| self.quiet_expr(a))
            }
            _ => false,
        }
    }

    /// The target of an assignment: a local, or an element or field reached from one.
    fn quiet_place(&mut self, p: &hir::Expr) -> bool {
        match &p.kind {
            H::Local(..) => true,
            H::Field { base, .. } => !self.proxiable(base.ty) && self.quiet_place(base),
            H::Index { base, index, .. } => self.quiet_place(base) && self.quiet_expr(index),
            _ => false,
        }
    }
}

/// Intrinsics that only read their arguments and compute: no user code (unlike `MutexWith`,
/// `JsonStringify`, `Print` or `ToString`, which call methods), no allocation, nothing dropped.
fn pure_intrinsic(i: hir::Intrinsic) -> bool {
    use hir::Intrinsic as I;
    matches!(
        i,
        I::ArrayLen
            | I::StrLen
            | I::StrByteLen
            | I::StrCharCodeAt
            | I::Sqrt
            | I::Floor
            | I::Ceil
            | I::Round
            | I::Trunc
            | I::FAbs
            | I::SimdShuffle(..)
    )
}
