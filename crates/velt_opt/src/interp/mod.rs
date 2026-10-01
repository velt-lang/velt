//! Reference interpreter for VIR (cargo feature `interp`).
//!
//! Executes any VIR program directly, with the semantics documented in `vir.rs`; the
//! optimizer's tests run programs before and after optimization and compare results and
//! extern-call traces. Useful project-wide as an oracle for lowering and codegen too.
//!
//! Model: every local (scalar or aggregate) gets a stack slot in a byte arena (`Memory`),
//! so `AddrOf`, `Deref` and `MemCopy` behave like on a real machine; statics are read-only
//! bytes with their relocations patched in at load time; function and extern addresses (`Const::Func`/`Const::Extern`) are opaque tokens that
//! indirect calls decode. Scalars travel as raw bits (see `Interp::call`). Extern calls go to
//! a `Host`, e.g. `RecordingHost`, which logs them. Errors (traps, malformed VIR, running out
//! of fuel) are reported as `Trap`s; the interpreter never panics on bad input.

mod exec;
mod memory;
mod scalar;

pub use memory::{Memory, StackMark};

use velt_vir::vir::{ExternFn, FuncId, Program, Ty};

/// Why execution stopped abnormally.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Trap {
    /// Reached an `Unreachable` terminator.
    Unreachable,
    /// A host reported that a noreturn extern (e.g. `velt_rt_panic`, `velt_rt_exit`) was called.
    NoReturn(String),
    /// Memory access outside any live allocation (or a write to a static).
    BadAddress(u64),
    /// Integer division or remainder by zero (lowering normally guards these).
    DivByZero,
    /// The step budget ran out (probably an infinite loop).
    OutOfFuel,
    /// Too much recursion or stack memory.
    StackOverflow,
    /// Indirect call to something that is not a function of the expected signature.
    BadCall(u64),
    /// The program violates a VIR invariant the interpreter relies on.
    Invalid(String),
}

/// The outside world: receives every extern call.
pub trait Host {
    /// Perform a call to `ext` with raw-bit `args`; return the raw-bit result (ignored for
    /// `Unit`). `mem` gives access to pointer arguments.
    fn call(&mut self, ext: &ExternFn, args: &[u64], mem: &mut Memory) -> Result<u64, Trap>;
}

/// One recorded argument of an extern call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Arg {
    /// Raw bits of a non-pointer scalar.
    Bits(u64),
    /// A pointer. Its value is not recorded: stack addresses legitimately differ between
    /// program versions (inlining changes frame layouts).
    Ptr,
}

/// One recorded extern call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExternCall {
    /// Symbol called.
    pub symbol: String,
    /// Arguments.
    pub args: Vec<Arg>,
}

/// A host that records every extern call, returns 0 from all of them, and traps with
/// `Trap::NoReturn` on noreturn externs.
#[derive(Debug, Default)]
pub struct RecordingHost {
    /// Calls so far, in order.
    pub calls: Vec<ExternCall>,
}

impl Host for RecordingHost {
    fn call(&mut self, ext: &ExternFn, args: &[u64], _mem: &mut Memory) -> Result<u64, Trap> {
        let args = ext
            .params
            .iter()
            .zip(args)
            .map(|(ty, &bits)| {
                if *ty == Ty::Ptr {
                    Arg::Ptr
                } else {
                    Arg::Bits(bits)
                }
            })
            .collect();
        self.calls.push(ExternCall {
            symbol: ext.symbol.clone(),
            args,
        });
        if ext.noreturn {
            return Err(Trap::NoReturn(ext.symbol.clone()));
        }
        Ok(0)
    }
}

/// Default step budget (statements + terminators executed).
const DEFAULT_FUEL: u64 = 200_000_000;
/// Maximum call depth.
const MAX_DEPTH: u32 = 10_000;

/// An interpreter instance for one program.
pub struct Interp<'p, H: Host> {
    program: &'p Program,
    /// Memory (inspectable after a run).
    pub mem: Memory,
    /// The host receiving extern calls.
    pub host: H,
    statics: Vec<u64>,
    fuel: u64,
    depth: u32,
}

impl<'p, H: Host> Interp<'p, H> {
    /// Load `program` (its statics are placed in memory). Malformed relocations (rejected by
    /// `velt_vir::verify`) are skipped or patched as null.
    pub fn new(program: &'p Program, host: H) -> Self {
        let mut mem = Memory::default();
        let statics = program
            .statics
            .iter()
            .map(|s| mem.add_static(&s.bytes, s.align))
            .collect();
        let mut interp = Interp {
            program,
            mem,
            host,
            statics,
            fuel: DEFAULT_FUEL,
            depth: 0,
        };
        interp.patch_relocs();
        interp
    }

    /// Set the remaining step budget.
    pub fn set_fuel(&mut self, fuel: u64) {
        self.fuel = fuel;
    }

    /// Remaining step budget.
    pub fn fuel(&self) -> u64 {
        self.fuel
    }

    /// Call function `id` with raw-bit scalar `args` (each the value's bit pattern at its
    /// type's width, zero-extended; `F32` as `f32` bits). Returns the raw-bit result (0 for
    /// `Unit`).
    pub fn call(&mut self, id: FuncId, args: &[u64]) -> Result<u64, Trap> {
        self.call_func(id, args)
    }

    /// Call the function with the given symbol.
    pub fn call_symbol(&mut self, symbol: &str, args: &[u64]) -> Result<u64, Trap> {
        let index = self
            .program
            .funcs
            .iter()
            .position(|f| f.symbol == symbol)
            .ok_or_else(|| Trap::Invalid(format!("no function `{symbol}`")))?;
        self.call_func(FuncId(index as u32), args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::builder::*;
    use velt_vir::vir::{BinOp, Const, Operand, Place, Rvalue, StaticId, Stmt, Ty::*};

    /// `f(shift)`: copies 8 bytes of a stack buffer onto itself shifted by `shift` with memcpy
    /// semantics, then returns whether the vtable slot holds `f`'s own address.
    fn program() -> Program {
        let mut pb = ProgramBuilder::new();
        let buf = pb.agg("Buf", 16, 8, &[(U64, 0), (U64, 8)]);
        let mut fb = FuncBuilder::export("f", &[U64], U64);
        let (b, p, q, slot) = (
            fb.local(Agg(buf)),
            fb.local(Ptr),
            fb.local(Ptr),
            fb.local(Ptr),
        );
        let (same, r) = (fb.local(Bool), fb.local(U64));
        let bb = fb.block();
        fb.assign(bb, p, Rvalue::AddrOf(Place::local(b)));
        fb.assign(
            bb,
            q,
            bin(BinOp::PtrAdd, copy_local(p), copy_local(fb.param(0))),
        );
        fb.push(
            bb,
            Stmt::MemCopyDyn {
                dst: copy_local(q),
                src: copy_local(p),
                len: int(8, U64),
                overlapping: false,
            },
        );
        let vtable = Operand::Const(Const::Static(StaticId(0)), Ptr);
        fb.assign(bb, p, Rvalue::Use(vtable));
        fb.assign(bb, slot, Rvalue::Use(copy_place(deref(p, Ptr))));
        let own = Operand::Const(Const::Func(FuncId(0)), Ptr);
        fb.assign(bb, same, bin(BinOp::Eq, copy_local(slot), own));
        fb.assign(bb, r, Rvalue::Cast(copy_local(same), U64));
        fb.ret(bb, copy_local(r));
        let f = pb.add(fb.finish());
        pb.stat_with(&[0; 8], 8, vec![(0, Const::Func(f))]);
        pb.finish()
    }

    #[test]
    fn relocations_hold_function_tokens_and_memcpy_overlap_traps() {
        let p = program();
        let mut interp = Interp::new(&p, RecordingHost::default());
        assert_eq!(interp.call(FuncId(0), &[8]), Ok(1));
        assert_eq!(
            interp.call(FuncId(0), &[4]),
            Err(Trap::Invalid("memcopy regions overlap".into()))
        );
    }
}
