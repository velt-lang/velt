//! The `velt_tls_*` externs (`velt_vir::tls`) as loads and stores of a `thread_local` global per
//! `ThreadLocal` constant, `@velt_tls_data_<id>`, which starts as the constant's starting value.
//! The unit that defines `velt_main` defines the globals (`hidden`, so other units reach them
//! directly) and the others declare them. `initialexec`: the program is the executable (or is
//! loaded with it), so a value is an offset from the thread pointer. LLVM strips thread-local
//! globals to plain ones for single-threaded WebAssembly.

use std::fmt::Write as _;

use velt_vir::tls::TlsCall;
use velt_vir::vir::{self, Callee, Ty};

use super::Emitter;
use crate::CodegenResult;

/// The definitions (`define`) or declarations of the program's thread-local globals.
pub(crate) fn tls_globals(program: &vir::Program, define: bool) -> String {
    let mut out = String::new();
    let mut seen = rustc_hash::FxHashSet::default();
    for e in &program.externs {
        let Some(call) = TlsCall::of_symbol(&e.symbol) else {
            continue;
        };
        if !seen.insert(call.id()) {
            continue;
        }
        let (bits, align) = (call.bits(), call.bits() / 8);
        let sym = call.data_symbol();
        let _ = if define {
            writeln!(
                out,
                "@{sym} = hidden thread_local(initialexec) global i{bits} {}, align {align}",
                signed(call.init(), bits)
            )
        } else {
            writeln!(
                out,
                "@{sym} = external hidden thread_local(initialexec) global i{bits}, align {align}"
            )
        };
    }
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

/// `w`, a `bits`-wide word, as LLVM prints an `i<bits>` constant.
fn signed(w: u64, bits: u32) -> i64 {
    let shift = 64 - bits;
    ((w << shift) as i64) >> shift
}

impl Emitter<'_> {
    /// The thread-local operation `callee` is, if it is a `velt_tls_*` extern.
    pub(super) fn tls_call(&self, callee: &Callee) -> Option<TlsCall> {
        let Callee::Extern(id) = callee else {
            return None;
        };
        TlsCall::of_symbol(&self.program.externs.get(id.0 as usize)?.symbol)
    }

    /// Emit `call` on `args`, returning its result, or `None` when it gives nothing.
    pub(super) fn tls(
        &mut self,
        call: TlsCall,
        params: &[Ty],
        ret: Ty,
        args: &[String],
    ) -> CodegenResult<Option<String>> {
        let (want_params, want_ret) = call.signature();
        if params != want_params || ret != want_ret {
            bail!(
                "ICE: `{}` declared as {params:?} -> {ret:?}, not {want_params:?} -> {want_ret:?}",
                call.symbol()
            );
        }
        self.intrinsics
            .need("declare ptr @llvm.threadlocal.address.p0(ptr)".into());
        let at = self.inst(format!(
            "call ptr @llvm.threadlocal.address.p0(ptr @{})",
            call.data_symbol()
        ));
        let (bits, align) = (call.bits(), call.bits() / 8);
        Ok(match call {
            TlsCall::Get { .. } => {
                Some(self.inst(format!("load i{bits}, ptr {at}, align {align}")))
            }
            TlsCall::Set { .. } => {
                self.line(format!(
                    "store i{bits} {}, ptr {at}, align {align}",
                    args[0]
                ));
                None
            }
        })
    }
}
