//! Translation of one VIR function into an LLVM IR `define`.
//!
//! Every non-unit local gets an `alloca` in the entry block and is accessed with plain
//! loads/stores: LLVM's `mem2reg`/SROA promote the scalar ones to SSA registers and split
//! aggregates, so this is both the simplest correct mapping and fully optimizable. Params are
//! stored into their locals on entry. Submodules handle one concern each on the shared
//! `Emitter`.

mod cast;
mod ops;
mod place;
mod stmt;
mod terminator;
mod value;

use std::fmt::Write;

use velt_vir::vir::{self, Ty};

use crate::debug::{DebugInfo, FnDebug};
use crate::module::{define_prefix, definition_params, Intrinsics, FN_ATTRS};
use crate::types::{global_name, scalar_type, size_align};
use crate::CodegenResult;

/// A resolved place: memory at `base + offset` holding a value of type `ty`.
#[derive(Clone, Debug)]
enum Loc {
    Mem {
        /// SSA name (or global) of the base pointer.
        base: String,
        offset: u64,
        /// Alignment known for `base`.
        base_align: u32,
        ty: Ty,
        /// The local whose alloca this is, when no pointer was dereferenced on the way (two
        /// places with different roots cannot overlap).
        root: Option<u32>,
    },
    Unit,
}

impl Loc {
    fn ty(&self) -> Ty {
        match self {
            Loc::Mem { ty, .. } => *ty,
            Loc::Unit => Ty::Unit,
        }
    }
}

/// A computed value.
#[derive(Clone, Debug)]
enum Val {
    /// SSA value or constant of a scalar type.
    Scalar(String),
    /// An aggregate in memory (copying it means memcpy/memmove).
    Agg(Loc),
    Unit,
}

/// Per-function translation state.
struct Emitter<'a> {
    program: &'a vir::Program,
    function: &'a vir::Function,
    intrinsics: &'a mut Intrinsics,
    /// Instructions emitted so far (labels included).
    body: String,
    next_temp: u32,
    /// Debug metadata of the module and this function (programs with source locations).
    debug: Option<(&'a mut DebugInfo, FnDebug)>,
    /// `, !dbg !N` appended to every instruction (empty without debug info).
    dbg_suffix: String,
    /// `Ptr` values occupy 8-byte `i64` slots in memory although machine pointers are
    /// narrower (`Target::wide_pointer_slots`, wasm32).
    wide_pointer_slots: bool,
}

/// Translate `function` into a complete `define ... { ... }` text.
pub(crate) fn emit_function(
    program: &vir::Program,
    function: &vir::Function,
    intrinsics: &mut Intrinsics,
    debug: Option<&mut DebugInfo>,
    wide_pointer_slots: bool,
) -> CodegenResult<String> {
    check_shape(function)?;
    let debug = debug.map(|d| {
        let f = d.function(function);
        (d, f)
    });
    let define_dbg = debug
        .as_ref()
        .map_or(String::new(), |(_, f)| DebugInfo::define_suffix(f));
    let mut emitter = Emitter {
        program,
        function,
        intrinsics,
        body: String::new(),
        next_temp: 0,
        debug,
        dbg_suffix: String::new(),
        wide_pointer_slots,
    };
    emitter.set_location(function.first_loc());
    emitter.entry_block()?;
    for (bi, block) in function.blocks.iter().enumerate() {
        emitter.label(&format!("bb{bi}"));
        for (si, s) in block.stmts.iter().enumerate() {
            emitter.set_location(function.loc(bi, si));
            emitter
                .stmt(s)
                .map_err(|e| format!("bb{bi} stmt {si} ({s:?}): {e}"))?;
        }
        emitter.set_location(function.term_loc(bi));
        emitter
            .terminator(&block.term)
            .map_err(|e| format!("bb{bi} terminator ({:?}): {e}", block.term))?;
    }
    Ok(format!(
        "{} {}({}) {FN_ATTRS}{define_dbg} {{\n{}}}\n",
        define_prefix(function)?,
        global_name(&function.symbol),
        definition_params(function)?,
        emitter.body
    ))
}

fn check_shape(function: &vir::Function) -> CodegenResult<()> {
    if function.blocks.is_empty() {
        bail!("function has no blocks");
    }
    if function.locals.len() < function.params.len() {
        bail!("fewer locals than parameters");
    }
    for (i, &param) in function.params.iter().enumerate() {
        let local = function.locals[i].ty;
        if local != param {
            bail!("local _{i} has type {local:?} but parameter {i} is {param:?}");
        }
    }
    Ok(())
}

impl Emitter<'_> {
    /// A separate entry block holds the allocas and spills the params: VIR block 0 may be a
    /// loop header, and LLVM's entry block cannot have predecessors.
    fn entry_block(&mut self) -> CodegenResult<()> {
        self.label("entry");
        let function = self.function;
        for (i, local) in function.locals.iter().enumerate() {
            match local.ty {
                Ty::Unit => {}
                Ty::Agg(_) => {
                    let (size, align) = size_align(self.program, local.ty)?;
                    self.line(format!(
                        "%l{i} = alloca [{} x i8], align {align}",
                        size.max(1)
                    ));
                }
                scalar => {
                    let (_, align) = size_align(self.program, scalar)?;
                    let t = self.memory_type(scalar);
                    self.line(format!("%l{i} = alloca {t}, align {align}"));
                }
            }
        }
        for (i, &ty) in function.params.iter().enumerate() {
            let (_, align) = size_align(self.program, ty)?;
            let v = self.widen_for_store(&format!("%p{i}"), ty);
            let t = self.memory_type(ty);
            self.line(format!("store {t} {v}, ptr %l{i}, align {align}"));
        }
        self.line("br label %bb0".into());
        Ok(())
    }

    /// The LLVM type a scalar of type `ty` has in memory.
    fn memory_type(&self, ty: Ty) -> &'static str {
        if self.wide_pointer_slots && ty == Ty::Ptr {
            "i64"
        } else {
            scalar_type(ty)
        }
    }

    /// Convert a register value of type `ty` to its [`Self::memory_type`] for a store.
    fn widen_for_store(&mut self, v: &str, ty: Ty) -> String {
        if self.wide_pointer_slots && ty == Ty::Ptr {
            self.inst(format!("ptrtoint ptr {v} to i64"))
        } else {
            v.to_string()
        }
    }

    /// Convert a loaded value of [`Self::memory_type`] back to a register value of type `ty`.
    fn narrow_after_load(&mut self, v: String, ty: Ty) -> String {
        if self.wide_pointer_slots && ty == Ty::Ptr {
            self.inst(format!("inttoptr i64 {v} to ptr"))
        } else {
            v
        }
    }

    /// Attach `at` (line 0 when unknown) to the instructions emitted from now on.
    fn set_location(&mut self, at: Option<vir::SrcLoc>) {
        if let Some((d, f)) = &mut self.debug {
            let id = d.location(f, at);
            self.dbg_suffix = format!(", !dbg !{id}");
        }
    }

    fn label(&mut self, name: &str) {
        let _ = writeln!(self.body, "{name}:");
    }

    /// Emit one instruction without a result.
    fn line(&mut self, text: String) {
        let _ = writeln!(self.body, "  {text}{}", self.dbg_suffix);
    }

    /// Emit `%tN = <text>` and return `%tN`.
    fn inst(&mut self, text: String) -> String {
        let name = format!("%t{}", self.next_temp);
        self.next_temp += 1;
        let _ = writeln!(self.body, "  {name} = {text}{}", self.dbg_suffix);
        name
    }

    fn block(&self, id: vir::BlockId) -> CodegenResult<String> {
        if (id.0 as usize) < self.function.blocks.len() {
            Ok(format!("%bb{}", id.0))
        } else {
            bail!("unknown block bb{}", id.0)
        }
    }
}
