//! Compile-time type tests (std/prelude/type_tests.vlt; sema `body/type_guards.rs`). A call of
//! the prelude's `typeIs<A, B>()` is the constant whether this instance's `A` and `B` are the
//! same type, so the `if` it is the condition of takes one branch, and the other is never
//! emitted (`branch` on a constant is a `goto`, and code in a block nothing reaches is dropped).
//! `asType<B>(x)` is `x` itself there (borrowed); where `A` and `B` differ it can only sit in such a dead
//! branch (sema checks its guard), and is a trap.

use velt_sema::hir::{self, DefId, TyId};

use super::FnLower;
use crate::vir::{Const, Operand, Place, Terminator, Ty};

const TYPE_IS: &str = "std/prelude/type_tests::typeIs";
const AS_TYPE: &str = "std/prelude/type_tests::asType";

impl FnLower<'_, '_> {
    /// The call of `typeIs` or `asType` (`def`, with this instance's type arguments `targs`)
    /// as what it means; `None` for any other function.
    pub(super) fn type_test_call(
        &mut self,
        def: DefId,
        targs: &[TyId],
        args: &[hir::Expr],
    ) -> Option<Operand> {
        let hir::Def::Fn(f) = self.cx.hir.def(def) else {
            return None;
        };
        match (f.name.as_str(), targs) {
            (TYPE_IS, [a, b]) => Some(Operand::Const(Const::Bool(a == b), Ty::Bool)),
            // Borrowed, as sema checked it: a consumer that keeps it counts it (`consume`).
            (AS_TYPE, [b, a]) if a == b => Some(self.expr(&args[0])),
            (AS_TYPE, [b, _]) => {
                let vt = self.cx.ty(*b);
                self.terminate(Terminator::Unreachable);
                let dead = self.new_block();
                self.switch_to(dead);
                Some(Operand::Copy(Place::local(self.temp(vt))))
            }
            _ => None,
        }
    }
}
