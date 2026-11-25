//! The module lock around what module state's accesses don't cover (rt_abi.md "Module state"):
//! a function's properties held as `any` (`Access::Touch`) and the `with`, `read` and `write`
//! callbacks that may reach them (`Access::With`, `Access::WithEnd`). The lock is the executor
//! in Velt's (std/lang/hooks/sched_module.velt); a program without the executor has one thread,
//! and its blocks no lock (module_blocks.velt): nothing to take. Which is known once the program
//! is built, so each call goes to a small function built last (`Work::ModuleAccess`).

use super::cint;
use super::hook_calls::{self as hooks, ty, HookTy};
use super::module_state::Access;
use super::{FnLower, Work};
use crate::vir::{self, Operand, Place, Terminator, Ty};

impl FnLower<'_, '_> {
    /// A call of module lock access `a` (`Touch`, `With`, `WithEnd`) on a native target.
    /// WebAssembly has one thread and no lock: callers make no call there.
    pub(super) fn module_lock(&mut self, a: Access, args: Vec<Operand>, dest: Option<Place>) {
        if !self.std_sched() {
            super::ice("a module lock call on a target without the executor in Velt");
        }
        let f = self.cx.func(Work::ModuleAccess(a));
        self.call(vir::Callee::Func(f), args, dest, false);
    }

    /// The body of lock access `a` in `self`, a bare function: its name, parameters and result.
    pub(super) fn module_lock_body(&mut self, a: Access) -> (&'static str, Vec<Ty>, Ty) {
        let sched = self.cx.module_sched();
        match a {
            Access::Touch => {
                if sched {
                    hooks::sched_module_touch(self).emit(self);
                }
                self.terminate(Terminator::Return(super::unit()));
                ("_Gmodlock_touch", vec![], Ty::Unit)
            }
            Access::With => {
                let took = match sched {
                    true => hooks::sched_module_with(self).emit(self).op(),
                    false => cint(0, Ty::U8),
                };
                self.terminate(Terminator::Return(took));
                ("_Gmodlock_with", vec![], Ty::U8)
            }
            Access::WithEnd => {
                let took = self.new_local(Ty::U8, Some("took".into()));
                if sched {
                    let t = ty::U8::arg(Operand::Copy(Place::local(took)));
                    hooks::sched_module_with_end(self, t).emit(self);
                }
                self.terminate(Terminator::Return(super::unit()));
                ("_Gmodlock_with_end", vec![Ty::U8], Ty::Unit)
            }
            _ => super::ice("a module state access is not a lock access"),
        }
    }
}
