//! Pass 1: definitions and signatures for every module-level item.
//!
//! Phases (each over all modules, so declaration order never matters):
//! 1. [`declare`]: a `DefId` per item, module scopes, imports ([`imports`], what modules export:
//!    [`exports`]), prelude exports.
//! 2. [`shapes`]: generic params + bounds, struct/class fields (base fields first), enum
//!    variants, interface members, `implements` lists.
//! 3. [`sigs`]: function / method / constructor / `extend` / interface-default signatures;
//!    [`iface_extends`] flattens interface inheritance.
//! 4. [`classes`]: `override` rules, vtable slots, inherited constructors, field-init rules;
//!    [`static_this`]: copies of the static methods using `this` per subclass.
//! 5. [`impls`]: `implements` checking and `Program::impls`; [`ext_impls`]: those of `extend …
//!    implements` blocks; [`comparable`]: `extend` blocks
//!    defining `compareTo` implement the builtin `Comparable<T>`; [`iterable`]: those defining
//!    `[Symbol.iterator](): Iterator<T, E>` implement `Iterable<T, E>`.
//!
//! [`field_only`]: field-only interfaces are object types (declared after phase 1, filled after
//! phase 3). Type-parameter defaults are resolved and checked after phase 1
//! (`crate::type_defaults`).
//!
//! [`lookup`] finds a class's methods (own, inherited, interface defaults) for all of the above.

mod abstract_fields;
mod abstracts;
pub(crate) mod c_sig;
mod classes;
mod comparable;
mod constants;
mod declare;
mod default_snapshots;
mod export_walk;
pub(crate) mod exports;
pub(crate) mod ext_fn_errors;
pub(crate) mod ext_impls;
mod extend_shadow;
pub(crate) mod extended_layouts;
mod field_only;
pub(crate) mod fill_defaults;
pub(crate) mod forwarders;
mod generator_sig;
pub(crate) mod getters;
pub(crate) mod host_sig;
pub(crate) mod iface_extends;
pub(crate) mod impls;
mod imports;
mod iterable;
mod lookup;
mod nested;
mod operators;
pub(crate) mod ret_infer;
pub(crate) mod shapes;
pub(crate) mod sigs;
pub(crate) mod static_this;
mod unsafe_methods;

use crate::ctx::Ctx;
use crate::hir::DefId;

pub(crate) use declare::fn_placeholder;
pub(crate) use declare::{ASYNC_DISPOSE, DISPOSE};
pub(crate) use exports::export_of;
pub(crate) use generator_sig::expr_result_args;
pub(crate) use lookup::{lookup_method, Found};
pub(crate) use nested::NestedItem;
pub(crate) use shapes::self_type;

/// The def allocated for each module-level and nested item.
pub(crate) type ItemDefs = Vec<DefId>;

/// Collect generated modules `ms` once the program's bodies are checked
/// (docs/internals/design/source-generators.md §6): their items declared in their own scope
/// (with their declaring module's names inherited) and their signatures resolved. Returns the
/// definitions made.
pub(crate) fn collect_generated(cx: &mut Ctx, ms: &[usize]) -> ItemDefs {
    let mut items = ItemDefs::new();
    declare::declare_generated(cx, ms, &mut items);
    sigs::resolve_def_sigs(cx, &items);
    sigs::fill_deferred(cx);
    items
}

pub(crate) fn collect(cx: &mut Ctx) -> ItemDefs {
    let mut items = ItemDefs::new();
    declare::declare_all(cx, &mut items);
    // Before any type is resolved: `any | null` is `any` (crate::unions).
    crate::any::add_impl(cx);
    collect_items(cx, &items, Added::default());
    cx.bodies_ok = true;
    sigs::fill_deferred(cx);
    items
}

/// Where what a collection adds starts: a whole program's starts at 0, that of std modules added
/// during the check (`crate::std_load`) after what is there, and the passes look only from here.
#[derive(Clone, Copy, Default)]
pub(crate) struct Added {
    pub defs: u32,
    pub extensions: usize,
    pub aliases: usize,
    pub overrides: usize,
    pub modules: usize,
}

/// Phases 2 to 5 of the module docs, for what starts at `from`.
fn collect_items(cx: &mut Ctx, items: &ItemDefs, from: Added) {
    cx.name_symbol_keys();
    field_only::declare(cx, from.defs);
    crate::type_defaults::check_from(cx, from);
    shapes::resolve_shapes(cx, items);
    cx.shapes_done = true;
    cx.check_deferred_ts_returns();
    sigs::resolve_sigs(cx, items, from.modules);
    iface_extends::flatten_from(cx, from.defs);
    cx.fill_synth_ifaces();
    field_only::fill(cx, from.defs);
    classes::check_classes(cx, from.defs);
    extend_shadow::check(cx, from);
    static_this::copy_statics(cx, from.defs);
    impls::build_impls(cx, from);
    ext_impls::build(cx, from.extensions);
    comparable::extension_impls(cx, from.extensions);
    iterable::extension_impls(cx, from.extensions);
    operators::check(cx, from);
}

/// Collect std modules `ms`, added to the check once its bodies are being reached
/// (`crate::std_load`): as [`collect`] does, for what they declare alone. They follow the (a1) rule
/// (velt_loader `std_index`), so nothing collected before changes. Returns where the additions
/// start.
pub(crate) fn collect_added(cx: &mut Ctx, ms: &[usize]) -> Added {
    let from = Added {
        defs: cx.info.len() as u32,
        extensions: cx.extensions.len(),
        aliases: cx.aliases.len(),
        overrides: cx.generic_overrides.len(),
        modules: ms.first().copied().unwrap_or(cx.modules.len()),
    };
    let mut items = ItemDefs::new();
    declare::declare_added(cx, ms, &mut items);
    // A utility type in a field type asks for fields while shapes are being resolved.
    cx.shapes_done = false;
    collect_items(cx, &items, from);
    sigs::fill_deferred(cx);
    cx.added_defs.push((from.defs, cx.info.len() as u32));
    from
}
