//! `Program::impls` and `Program::extension_impls`' bounds.

use super::{DefId, TyId};

/// A bound on type parameter `param` of a generic impl: the type bound to it implements
/// `iface<args>` (`args` use the impl's `TyKind::Param`s).
#[derive(Clone, Debug)]
pub struct ImplBound {
    pub param: u32,
    pub iface: DefId,
    pub args: Vec<TyId>,
}

/// `ty` implements `iface<iface_args>` using `methods` (one per interface method, in order).
/// Generic impls use `TyKind::Param` in `ty`/`iface_args`; lowering instantiates them.
#[derive(Clone, Debug)]
pub struct ImplDef {
    pub ty: TyId,
    pub generics: u32,
    pub iface: DefId,
    pub iface_args: Vec<TyId>,
    pub methods: Vec<DefId>,
}
