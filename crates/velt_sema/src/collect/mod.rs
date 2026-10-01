//! Pass 1: definitions and signatures for every module-level item.
//!
//! Phases (each over all modules, so declaration order never matters):
//! 1. [`declare`]: a `DefId` per item, module scopes, imports ([`imports`], what modules export:
//!    [`exports`]), prelude exports.
//! 2. [`shapes`]: generic params + bounds, struct/class fields (base fields first), enum
//!    variants, interface members, `implements` lists.
//! 3. [`sigs`]: function / method / constructor / `extend` / interface-default signatures;
//!    [`iface_extends`] flattens interface inheritance.
//! 4. [`classes`]: `override` rules, vtable slots, inherited constructors, field-init rules.
//! 5. [`impls`]: `implements` checking and `Program::impls`; [`comparable`]: `extend` blocks
//!    defining `compareTo` implement the builtin `Comparable<T>`.
//!
//! [`lookup`] finds a class's methods (own, inherited, interface defaults) for all of the above.

mod classes;
mod comparable;
mod constants;
mod declare;
mod exports;
mod forwarders;
mod getters;
mod iface_extends;
mod impls;
mod imports;
mod lookup;
mod nested;
mod shapes;
mod sigs;

use crate::ctx::Ctx;
use crate::hir::DefId;

pub(crate) use declare::fn_placeholder;
pub(crate) use declare::{ASYNC_DISPOSE, DISPOSE};
pub(crate) use exports::export_of;
pub(crate) use lookup::{lookup_method, Found};
pub(crate) use nested::NestedItem;
pub(crate) use shapes::self_type;

/// The def allocated for each module-level and nested item.
pub(crate) type ItemDefs = Vec<DefId>;

pub(crate) fn collect(cx: &mut Ctx) -> ItemDefs {
    let mut items = ItemDefs::new();
    declare::declare_all(cx, &mut items);
    shapes::resolve_shapes(cx, &items);
    sigs::resolve_sigs(cx, &items);
    iface_extends::flatten_all(cx);
    classes::check_classes(cx);
    impls::build_impls(cx);
    comparable::extension_impls(cx);
    items
}
