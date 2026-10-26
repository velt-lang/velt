//! `Program::extension_impls`' bounds.

use super::{DefId, TyId};

/// A bound on type parameter `param` of a generic impl: the type bound to it implements
/// `iface<args>` (`args` use the impl's `TyKind::Param`s).
#[derive(Clone, Debug)]
pub struct ImplBound {
    pub param: u32,
    pub iface: DefId,
    pub args: Vec<TyId>,
}
