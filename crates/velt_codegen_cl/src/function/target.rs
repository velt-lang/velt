//! What translating a function reads of the module, as a copy that worker threads share while
//! the module itself takes the compiled code (module.rs): the target ISA, what the declarations
//! say about each function and data object, and the C library functions declared so far.

use std::sync::OnceLock;

use cranelift_codegen::ir;
use cranelift_codegen::isa::{TargetFrontendConfig, TargetIsa};
use cranelift_module::{DataId, FuncId, Module};

/// The module's declarations as translation needs them. `declare_*_in_func` make what
/// `cranelift_module::Module`'s do (which read only the declarations, but borrow the module).
pub(crate) struct Target<'a> {
    isa: &'a dyn TargetIsa,
    /// Per function: its signature, and whether it is defined in this module (`colocated`).
    funcs: Vec<(ir::Signature, bool)>,
    /// Per data object: `colocated` and thread-local.
    data: Vec<(bool, bool)>,
    /// The C library functions declared after the copy was made, with their signatures.
    libs: [OnceLock<(FuncId, ir::Signature)>; LibFunction::COUNT],
}

impl<'a> Target<'a> {
    /// A copy of `module`'s declarations, for code generated for `isa` (the module's).
    pub(crate) fn of<M: Module>(isa: &'a dyn TargetIsa, module: &M) -> Self {
        let declarations = module.declarations();
        let funcs = (declarations.get_functions())
            .map(|(_, decl)| (decl.signature.clone(), decl.linkage.is_final()))
            .collect();
        let data = (declarations.get_data_objects())
            .map(|(_, decl)| (decl.linkage.is_final(), decl.tls))
            .collect();
        Target {
            isa,
            funcs,
            data,
            libs: Default::default(),
        }
    }

    pub(crate) fn isa(&self) -> &'a dyn TargetIsa {
        self.isa
    }

    pub(crate) fn target_config(&self) -> TargetFrontendConfig {
        self.isa.frontend_config()
    }

    /// The id of C library function `lib`, once the module declared it.
    pub(crate) fn lib(&self, lib: LibFunction) -> Option<FuncId> {
        self.libs[lib as usize].get().map(|(id, _)| *id)
    }

    /// Record that the module declared `lib` as `id` with `signature` (an import).
    pub(crate) fn add_lib(&self, lib: LibFunction, id: FuncId, signature: ir::Signature) {
        let _ = self.libs[lib as usize].set((id, signature));
    }

    /// A reference to function `func_id` from `func`, as `Module::declare_func_in_func` makes it.
    pub(crate) fn declare_func_in_func(
        &self,
        func_id: FuncId,
        func: &mut ir::Function,
    ) -> ir::FuncRef {
        let (signature, colocated) = match self.funcs.get(func_id.as_u32() as usize) {
            Some((signature, colocated)) => (signature.clone(), *colocated),
            None => {
                let lib = self.libs.iter().filter_map(OnceLock::get);
                let found = lib.into_iter().find(|(id, _)| *id == func_id);
                let Some((_, signature)) = found else {
                    panic!("ICE: function {func_id} is not declared");
                };
                // An import: not defined in this module.
                (signature.clone(), false)
            }
        };
        let signature = func.import_signature(signature);
        let user_name_ref = func.declare_imported_user_function(ir::UserExternalName {
            namespace: 0,
            index: func_id.as_u32(),
        });
        func.import_function(ir::ExtFuncData {
            name: ir::ExternalName::user(user_name_ref),
            signature,
            colocated,
            patchable: false,
        })
    }

    /// A global value of data object `data` in `func`, as `Module::declare_data_in_func` makes it.
    pub(crate) fn declare_data_in_func(
        &self,
        data: DataId,
        func: &mut ir::Function,
    ) -> ir::GlobalValue {
        let Some(&(colocated, tls)) = self.data.get(data.as_u32() as usize) else {
            panic!("ICE: data object {data} is not declared");
        };
        let user_name_ref = func.declare_imported_user_function(ir::UserExternalName {
            namespace: 1,
            index: data.as_u32(),
        });
        func.create_global_value(ir::GlobalValueData::Symbol {
            name: ir::ExternalName::user(user_name_ref),
            offset: ir::immediates::Imm64::new(0),
            colocated,
            tls,
        })
    }
}

/// A C library function that translation may call (Cranelift has no instruction for it). The
/// module declares one when a function first needs it, so an object imports only those its code
/// calls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LibFunction {
    Fmod,
    Fmodf,
}

impl LibFunction {
    const COUNT: usize = 2;

    pub(crate) fn name(self) -> &'static str {
        match self {
            LibFunction::Fmod => "fmod",
            LibFunction::Fmodf => "fmodf",
        }
    }

    /// Its parameters' and result's type.
    pub(crate) fn ty(self) -> velt_vir::vir::Ty {
        match self {
            LibFunction::Fmod => velt_vir::vir::Ty::F64,
            LibFunction::Fmodf => velt_vir::vir::Ty::F32,
        }
    }
}
