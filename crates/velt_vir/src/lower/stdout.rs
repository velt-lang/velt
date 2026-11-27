//! Standard output and standard error through std's hooks (std/lang/hooks/stdout.velt, #2196):
//! the writers `console.log`, `console.error` and an uncaught error's report call, and the flush
//! before every exit Velt code reaches (the end of `velt_main`, `process.exit`, a panic). The
//! writer behind them is the runtime's for now (out_rt.velt).

use super::hook_calls::{self as hooks, ty, HookTy};
use super::{cint, FnLower};
use crate::vir::{Operand, Ty};

/// The std modules of the writer itself (std/lang/hooks/stdout*.velt, out_*.velt): their panics
/// and exits don't flush first, so a panic inside the flush doesn't call the flush again.
const WRITER_MODULES: [&str; 2] = ["std/lang/hooks/stdout", "std/lang/hooks/out_"];

impl FnLower<'_, '_> {
    /// Writes the string at address `s` to `stream` (1 = stdout, 2 = stderr).
    pub(super) fn write_str(&mut self, stream: &Operand, s: Operand) {
        let stream = ty::U32::arg(stream.clone());
        hooks::write_str(self, stream, ty::Str::arg(s)).emit(self);
    }

    /// Writes `true` or `false`.
    pub(super) fn write_bool(&mut self, stream: &Operand, v: Operand) {
        let stream = ty::U32::arg(stream.clone());
        hooks::write_bool(self, stream, ty::Bool::arg(v)).emit(self);
    }

    /// Writes the byte `b` (the space or newline after a `console.log` argument).
    pub(super) fn write_byte(&mut self, stream: &Operand, b: u8) {
        let (stream, b) = (ty::U32::arg(stream.clone()), cint(b as i128, Ty::U8));
        hooks::write_byte(self, stream, ty::U8::arg(b)).emit(self);
    }

    /// The flush before the process ends: what stdout holds goes to the OS. Nothing where the
    /// code is unreachable, or in the writer's own code.
    pub(super) fn flush_for_exit(&mut self) {
        if self.dead() || self.in_writer() {
            return;
        }
        hooks::flush_for_exit(self).emit(self);
    }

    /// `velt_main`'s start: stdout's mode (a terminal or not) is asked once, before any task
    /// writes, so that the first `console.log` of a task doesn't wait for the answer; and outside
    /// the dev JIT, whose reload replaces the code a C library's exit handler would call, std's
    /// flush is registered with `atexit`, for the exits the runtime takes by itself.
    pub(super) fn init_output(&mut self) {
        hooks::init_output(self).emit(self);
        if !self.cx.jit {
            hooks::flush_at_exit(self).emit(self);
        }
    }

    /// Is the function being lowered part of the writer (`WRITER_MODULES`)?
    fn in_writer(&self) -> bool {
        let Some(d) = self.cx.cur_def else {
            return false;
        };
        match self.cx.hir.def(d) {
            velt_sema::hir::Def::Fn(f) => WRITER_MODULES.iter().any(|m| f.name.starts_with(m)),
            _ => false,
        }
    }
}

impl super::Cx<'_> {
    /// The hooks a prebuilt object may call in the program that links it (velt_common
    /// `prebuilt::calls_out`: the writer's entries), lowered on a native target whatever the
    /// program itself calls, so that a program linking an object defines them (`veltc` exports
    /// them then); the optimizer drops each one nothing calls.
    pub(super) fn seed_calls_out(&mut self) {
        if super::c_call::wasm32(&self.target) {
            return;
        }
        for h in velt_common::std_hooks::prebuilt::calls_out() {
            let def =
                (self.prelude_def(&h.qualified())).filter(|d| !self.hir.unchecked.contains(d));
            if let Some(d) = def {
                self.func(super::Work::Fn(d, vec![]));
            }
        }
    }
}
