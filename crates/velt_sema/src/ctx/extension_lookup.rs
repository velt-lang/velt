//! What method lookup through `extend` blocks keeps during a check (`Ctx::ext_lookup`).

use rustc_hash::{FxHashMap, FxHashSet};

use crate::hir::TyId;

/// The method-name index of the `extend` blocks, and the blocks instance calls on built-in vector
/// types resolved to.
#[derive(Clone, Default)]
pub(crate) struct ExtensionLookup {
    /// The extensions declaring each method name, built for the first `.0` extensions
    /// (body/expr/method.rs `applicable_extensions`).
    pub names: (usize, FxHashMap<String, Vec<usize>>),
    /// The blocks instance calls on built-in vector types resolved to, with the vector type
    /// (`FnCx::extension_method`): a block added in the middle of the check may not take one over
    /// (`collect::vector_extends`).
    pub vector_calls: FxHashSet<(usize, TyId)>,
}
