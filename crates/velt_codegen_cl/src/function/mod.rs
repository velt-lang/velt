//! Translation of one VIR function into Cranelift IR.
//!
//! Scalar locals whose address is never taken become `cranelift_frontend::Variable`s (SSA
//! construction is left to `FunctionBuilder`); aggregates and address-taken scalars live in
//! explicit stack slots. Submodules handle one concern each on the shared `Translator`.

mod binary;
mod cast;
mod operand;
mod place;
mod stmt;
mod terminator;

use std::collections::HashMap;

use cranelift_codegen::ir::{self, InstBuilder, StackSlotData, StackSlotKind};
use cranelift_codegen::isa::TargetFrontendConfig;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_module::{DataId, FuncId, Module};
use target_lexicon::{Architecture, BinaryFormat};
use velt_vir::vir::{self, BlockId, Place, Proj, Rvalue, Stmt, Ty};

use crate::abi::{scalar_type, size_align};
use crate::module::Declarations;
use crate::CodegenResult;

/// C library functions imported on demand (float remainder), shared across functions.
#[derive(Default)]
pub(crate) struct LibFunctions {
    fmod: Option<FuncId>,
    fmodf: Option<FuncId>,
}

/// Where a local lives.
#[derive(Clone, Copy)]
enum Storage {
    Var(Variable),
    Slot(ir::StackSlot),
    Unit,
}

/// A resolved place.
#[derive(Clone, Copy)]
enum Loc {
    Var(Variable, Ty),
    /// Memory at `base + offset` holding a value of type `Ty`.
    Mem(ir::Value, i32, Ty),
    Unit,
}

impl Loc {
    fn ty(&self) -> Ty {
        match *self {
            Loc::Var(_, t) | Loc::Mem(_, _, t) => t,
            Loc::Unit => Ty::Unit,
        }
    }
}

/// A computed value.
#[derive(Clone, Copy)]
enum Val {
    Scalar(ir::Value),
    /// Address of an aggregate in memory (copying it means memcpy).
    Agg(ir::Value),
    Unit,
}

/// Per-function translation state.
struct Translator<'a, 'b, M: Module> {
    module: &'a mut M,
    program: &'a vir::Program,
    decls: &'a Declarations,
    libs: &'a mut LibFunctions,
    function: &'a vir::Function,
    builder: FunctionBuilder<'b>,
    frontend_config: TargetFrontendConfig,
    blocks: Vec<ir::Block>,
    storage: Vec<Storage>,
    func_refs: HashMap<FuncId, ir::FuncRef>,
    /// Function refs used only to take addresses (see `far_addresses`).
    addr_refs: HashMap<FuncId, ir::FuncRef>,
    data_refs: HashMap<DataId, ir::GlobalValue>,
    /// Materialize symbol addresses with absolute relocations instead of PC-relative page
    /// addressing: cranelift-object cannot emit ADRP relocations for aarch64 COFF.
    far_addresses: bool,
}

/// Translate `function` into `func` (whose signature and name are already set).
pub(crate) fn translate_function<M: Module>(
    module: &mut M,
    program: &vir::Program,
    decls: &Declarations,
    libs: &mut LibFunctions,
    function: &vir::Function,
    func: &mut ir::Function,
    builder_ctx: &mut FunctionBuilderContext,
) -> CodegenResult<()> {
    check_shape(function)?;
    let in_memory = memory_locals(function)?;
    let frontend_config = module.target_config();
    let triple = module.isa().triple();
    let far_addresses = matches!(triple.architecture, Architecture::Aarch64(_))
        && triple.binary_format == BinaryFormat::Coff;
    let mut builder = FunctionBuilder::new(func, builder_ctx);
    let storage = allocate_locals(&mut builder, program, function, &in_memory)?;
    let mut translator = Translator {
        module,
        program,
        decls,
        libs,
        function,
        builder,
        frontend_config,
        blocks: Vec::new(),
        storage,
        func_refs: HashMap::new(),
        addr_refs: HashMap::new(),
        data_refs: HashMap::new(),
        far_addresses,
    };
    translator.run()?;
    translator.builder.seal_all_blocks();
    translator.builder.finalize();
    Ok(())
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

/// Which locals must live in memory: aggregates, and scalars whose address is taken
/// (an `AddrOf` place that does not start by dereferencing the local).
fn memory_locals(function: &vir::Function) -> CodegenResult<Vec<bool>> {
    let mut in_memory: Vec<bool> = function
        .locals
        .iter()
        .map(|l| matches!(l.ty, Ty::Agg(_)))
        .collect();
    let addr_of = function
        .blocks
        .iter()
        .flat_map(|b| &b.stmts)
        .filter_map(|s| match s {
            Stmt::Assign(_, Rvalue::AddrOf(p)) => Some(p),
            _ => None,
        });
    for place in addr_of {
        if matches!(place.proj.first(), Some(Proj::Deref(_))) {
            continue;
        }
        match in_memory.get_mut(place.local.0 as usize) {
            Some(m) => *m = true,
            None => bail!("AddrOf of unknown local _{}", place.local.0),
        }
    }
    Ok(in_memory)
}

fn allocate_locals(
    builder: &mut FunctionBuilder,
    program: &vir::Program,
    function: &vir::Function,
    in_memory: &[bool],
) -> CodegenResult<Vec<Storage>> {
    let mut storage = Vec::with_capacity(function.locals.len());
    for (i, local) in function.locals.iter().enumerate() {
        storage.push(match local.ty {
            Ty::Unit => Storage::Unit,
            ty if in_memory[i] => {
                let (size, align) = size_align(program, ty)?;
                let slot = builder.create_sized_stack_slot(StackSlotData::new(
                    StackSlotKind::ExplicitSlot,
                    size.max(1),
                    align.trailing_zeros() as u8,
                ));
                Storage::Slot(slot)
            }
            ty => Storage::Var(builder.declare_var(scalar_type(ty))),
        });
    }
    Ok(storage)
}

impl<M: Module> Translator<'_, '_, M> {
    fn run(&mut self) -> CodegenResult<()> {
        // A separate entry block: VIR block 0 may be a loop header, and Cranelift's entry
        // block cannot have predecessors.
        let entry = self.builder.create_block();
        self.builder.append_block_params_for_function_params(entry);
        self.blocks = (0..self.function.blocks.len())
            .map(|_| self.builder.create_block())
            .collect();
        self.builder.switch_to_block(entry);
        let params = self.builder.block_params(entry).to_vec();
        for (i, value) in params.into_iter().enumerate() {
            let loc = self.place(&Place::local(vir::Local(i as u32)))?;
            self.write(loc, Val::Scalar(value))?;
        }
        let first = self.blocks[0];
        self.builder.ins().jump(first, &[]);

        let function = self.function;
        for (bi, block) in function.blocks.iter().enumerate() {
            self.builder.switch_to_block(self.blocks[bi]);
            for (si, s) in block.stmts.iter().enumerate() {
                self.stmt(s)
                    .map_err(|e| format!("bb{bi} stmt {si} ({s:?}): {e}"))?;
            }
            self.terminator(&block.term)
                .map_err(|e| format!("bb{bi} terminator ({:?}): {e}", block.term))?;
        }
        Ok(())
    }

    fn block(&self, id: BlockId) -> CodegenResult<ir::Block> {
        match self.blocks.get(id.0 as usize) {
            Some(b) => Ok(*b),
            None => bail!("unknown block bb{}", id.0),
        }
    }
}
