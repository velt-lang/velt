//! What method lookup through `extend` blocks keeps during a check (`Ctx::ext_lookup`).

use rustc_hash::{FxHashMap, FxHashSet};

use crate::hir::TyId;

/// The method-name index of the `extend` blocks, and the method names looked up on built-in vector
/// types.
#[derive(Clone, Default)]
pub(crate) struct ExtensionLookup {
    /// The extensions declaring each method name, built for the first `.0` extensions
    /// (body/expr/method.rs `applicable_extensions`).
    pub names: (usize, FxHashMap<String, Vec<usize>>),
    /// The names looked up through `extend` blocks on each built-in vector type, found or not
    /// (`FnCx::extension_method`, `FnCx::static_op`; an operator is the lookup of its method): a
    /// block added in the middle of the check may not answer one differently
    /// (`collect::vector_extends`).
    pub vector_lookups: FxHashSet<(TyId, String)>,
}
