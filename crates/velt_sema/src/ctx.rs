//! Whole-program checking context shared by all passes: definitions, module scopes, types.

use rustc_hash::{FxHashMap, FxHashSet};

use velt_common::{Diagnostic, Diagnostics, Span};

use crate::defs::{
    AdtInfo, AliasInfo, DefInfo, EnumInfo, Extension, FnInfo, GlobalInfo, IfaceInfo,
};
use crate::hir::{AdtKind, Def, DefId, ImplDef, LocalId, TyId, TyKind};
use crate::types::Types;
use crate::SourceModule;

/// A name visible at module level.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Item {
    Def(DefId),
    Alias(u32),
}

#[derive(Default)]
pub(crate) struct ModuleScope {
    /// Own items and imports.
    pub items: FxHashMap<String, Item>,
    /// Names declared with `export` (lists and re-exports: `crate::collect::exports`).
    pub exports: FxHashSet<String>,
    /// `import * as ns`: namespace name → module index (its exports are bound as `ns.x`).
    pub namespaces: FxHashMap<String, usize>,
    /// Names bound by `import type` (types only: using one as a value is an error).
    pub type_only: FxHashSet<String>,
    /// `std/...` module: may call `__intrinsic_*`.
    pub is_std: bool,
}

/// The classes below a class, deepest first, each with the method its vtable calls at a slot
/// (`Ctx::classes_below`).
pub(crate) type ClassesBelow = std::rc::Rc<[(DefId, DefId)]>;

pub(crate) struct Ctx<'m> {
    pub modules: &'m [SourceModule],
    pub root: usize,
    pub ty: Types,
    /// Final HIR definitions, indexed by `DefId` (filled at the end).
    pub defs: Vec<Option<Def>>,
    /// Sema information per `DefId`.
    pub info: Vec<DefInfo<'m>>,
    pub def_spans: Vec<Span>,
    pub scopes: Vec<ModuleScope>,
    /// Exports of `std/prelude/*` modules, visible everywhere.
    pub prelude: FxHashMap<String, Item>,
    pub aliases: Vec<AliasInfo<'m>>,
    pub extensions: Vec<Extension>,
    pub impls: Vec<ImplDef>,
    /// Interfaces written through and their setter slots (`crate::iface_writes`).
    pub(crate) iface_writes: crate::iface_writes::IfaceWrites,
    pub(crate) part_ifaces: crate::intersection_members::PartIfaces,
    /// Interfaces made for intersections and `Pick`s of interfaces with methods.
    pub(crate) synth_ifaces: crate::synth_ifaces::SynthIfaces,
    /// Views of unions of interfaces (`crate::union_views`).
    pub(crate) union_views: crate::union_views::UnionViews,
    pub(crate) any: Option<crate::any::AnyDefs>,
    /// `impls` by interface (`Ctx::find_impl`).
    pub impl_index: crate::infer::ImplIndex,
    /// Impls from `extend … implements I` blocks: impl index → extension index
    /// (collect/ext_impls.rs). Their bounds must hold for an impl to fit a type, and among the
    /// fitting ones the most specific wins (`Ctx::find_impl`).
    pub ext_impls: FxHashMap<u32, usize>,
    /// `ext_impls` by interface, in order.
    pub ext_by_iface: FxHashMap<DefId, Vec<u32>>,
    /// Finished `extend` lookups (infer/ext_impls.rs) and bound checks through blocks
    /// (infer/ext_exists.rs), valid while `impls` has `ext_memo_impls` entries.
    pub ext_memo: FxHashMap<(TyId, DefId), Option<crate::infer::FoundImpl>>,
    pub ext_exists_memo: FxHashMap<crate::infer::ExistsKey, bool>,
    pub ext_memo_impls: usize,
    /// Set while `Ctx::blanket_bounds` checks a blanket impl's bounds: the bounds a parameter
    /// has through blanket impls come from its declared bounds only, not from other blanket
    /// impls, so the check never recurses.
    pub in_blanket: bool,
    /// The type-parameter bounds in scope of the `satisfies` call running `find_impl`: an
    /// `extend` impl's bounds are checked in it (`Ctx::fitting_impl`).
    pub impl_scope: Vec<Vec<crate::defs::Bound>>,
    /// Anonymous object defs by shape: field names, declared types and flags, in order.
    pub anon: FxHashMap<Vec<crate::anon::ShapeField>, DefId>,
    /// Object type defs replaced before lowering (`crate::readonly`): anonymous ones with
    /// `readonly` fields → their twin without, and field-only interfaces' object types → the
    /// anonymous object type of their fields. With a template, the twin's type arguments are
    /// the template's types with the original arguments substituted (a param order change).
    pub readonly_twins: FxHashMap<DefId, (DefId, Option<Vec<TyId>>)>,
    /// A container of `any` was used as one of `unknown` or back (`same_but_any_unknown`):
    /// `unknown` is erased to `any` before lowering (`crate::readonly`), so both are one type.
    pub any_unknown_views: bool,
    /// What the `Object` built-ins make: frozen types, prototype-less records.
    pub objects: crate::anon::ObjectState,
    /// Branded type definitions → their primitive (`crate::brands`).
    pub brands: FxHashMap<DefId, TyId>,
    /// (primitive, object part) → the brand's definition.
    pub brand_keys: FxHashMap<(TyId, TyId), DefId>,
    /// Brands whose object part mentions type parameters (`symbol & { __t?: T }`): substitution
    /// brands the substituted object part (`Ctx::subst`). Empty in most programs.
    pub generic_brands: FxHashSet<DefId>,
    /// Brands of a `Record<string, any>` (or `unknown`) by the named members written next to
    /// its index signature → that object type of the named members (`crate::index_signatures`).
    pub record_views: FxHashMap<DefId, TyId>,
    /// Field-only interface → its object type's def (`collect::field_only`).
    pub field_only: FxHashMap<DefId, DefId>,
    /// The reverse of `field_only`.
    pub field_only_of: FxHashMap<DefId, DefId>,
    /// Every declared type's fields are resolved (`collect::shapes`); before that, a utility
    /// type (`crate::utility_types`) may only read the fields of types already shaped.
    pub shapes_done: bool,
    /// Type queries (`crate::type_queries`): the locals a body's type annotation may name
    /// (`typeof x`), set by the body checker around each annotation it resolves.
    pub query_locals: Vec<(String, TyId)>,
    /// Collection is finished, so a type query may check a body (a function's inferred result,
    /// a constant without a type annotation).
    pub bodies_ok: bool,
    /// Free functions' signatures that wait for a body (`crate::type_queries::SigQueries`).
    pub sigq: crate::type_queries::SigQueries<'m>,
    /// Checking type aliases nothing uses (`resolve::check_unused_aliases`): a utility type on
    /// one of the alias's own type parameters is fine there (it is resolved at each use).
    pub checking_unused_aliases: bool,
    /// Some body has a type predicate on a type parameter's value (`MayHoldPrimitive`, #1251):
    /// error inference then drops what a guarded branch throws in object instances.
    pub primitive_guards: bool,
    /// The state of type-level evaluation (`crate::type_level`).
    pub(crate) type_level: crate::type_level::State,
    /// The program (not std) writes `undefined`: else the checks only it needs (`undefined_args`,
    /// a `toJSON()` returning `undefined`) are skipped (the prelude uses `T | undefined` itself).
    pub writes_undefined: bool,
    /// Type definitions whose shape (`collect::shapes`) is resolved, and those being resolved now:
    /// a utility type in a field type shapes the type it reads first, whatever the order.
    pub shaped: FxHashSet<DefId>,
    pub shaping: Vec<DefId>,
    /// Classes whose fields are laid out base-first (`collect::shapes::layout_fields`).
    pub laid_out: FxHashSet<DefId>,
    /// Canonical form of each type canonicalized so far (`crate::anon`, `Ctx::canon`).
    pub canon_memo: FxHashMap<TyId, TyId>,
    /// Object types `crate::replaced_holders` accepted assignments for because they are shared:
    /// lowering counts them from its first pass (`hir::Program::counted_objects`).
    pub counted_objects: Vec<TyId>,
    /// Concrete union defs readonly erasure replaced (`crate::union_layouts::Plan::replaced`).
    pub replaced_unions: FxHashSet<DefId>,
    pub module_slots: Vec<DefId>, // module state, in initialization order (module_state.rs)
    pub module_scripts: Vec<DefId>, // imported modules' top-level statements (module_scripts.rs)
    pub promoted_consts: FxHashSet<Span>, // `const`s kept in module state (module_chunks.rs)
    /// The properties of `globalThis` the program uses (`crate::global_this`).
    pub global_props: crate::global_this::GlobalProps,
    /// Module constants found to depend on themselves so far (`body::driver::ensure_global`).
    pub global_cycles: u32,
    /// Union enums by canonical member list (`crate::unions`).
    pub unions: FxHashMap<Vec<TyId>, DefId>,
    pub recursive_unions: FxHashMap<u32, Option<TyId>>, // by alias (crate::recursive_unions)
    /// Names of type aliases for structural types (`type Shape = A | B`), for messages.
    pub alias_names: FxHashMap<TyId, String>,
    /// (class, method key) of every `override` of a generic method (no vtable slot).
    pub generic_overrides: Vec<(DefId, String)>,
    /// The throwing setters a getter-only override gets (`collect::classes`): a write through
    /// the class itself is a type error, as in TypeScript, one through a base class throws.
    pub getter_only_setters: FxHashSet<DefId>,
    /// Names of `Param(i)` in the definition being checked (for messages).
    pub display_params: Vec<String>,
    /// Functions whose parameter defaults are checked (or being checked): they are checked on
    /// first use, since a field default or constant may call with fewer arguments.
    pub defaults_checked: FxHashSet<DefId>,
    /// Per class and vtable slot: the classes below it, deepest first, with the method each
    /// one's vtable calls there (`body::expr::virtual_defaults`).
    pub classes_below: FxHashMap<(DefId, u32), ClassesBelow>,
    /// The default-filling vtable entries (`collect::fill_defaults`).
    pub fills: FxHashMap<DefId, crate::collect::fill_defaults::Fill>,
    /// What each parameter default that may throw throws, by its span (`defaults::check_default`).
    pub default_throws: FxHashMap<Span, Vec<crate::defs::ThrowSrc>>,
    /// Implementations of accessor interfaces for object types (`body::expr::iface_from_object`).
    pub object_impls: crate::body::expr::iface_from_object::ObjectImpls,
    /// The adapter of each `toString` / `toJSON` method glue calls through one (`crate::hooks`).
    pub hook_adapters: FxHashMap<DefId, DefId>,
    /// Types whose field defaults are checked (or being checked): checked up front, or on first
    /// use by a `new` or struct literal in a default checked before them.
    pub field_defaults_checked: FxHashSet<DefId>,
    /// Closure counters per top-level function name.
    pub closure_counts: FxHashMap<String, u32>,
    /// Every function-like def, in creation order.
    pub fn_defs: Vec<DefId>,
    /// Named functions used as values (`FnRef`) with type arguments, checked after ownership.
    pub fn_values: Vec<(DefId, Vec<TyId>, Span)>,
    /// Declarations of the `for…of` bindings over a value read out of a `shared` (or what a
    /// method of one returns): the ownership pass refuses changing them
    /// (`ownership::finish::frozen_bindings`).
    pub frozen_elems: FxHashSet<Span>,
    /// Declarations of `for…of` bindings, with the local whose value the loop walks: a function
    /// changing such an element changes that local's value (`FnInfo::modified_params`; a map
    /// loop's bindings are not bound into its source, `ownership::evidence`).
    pub loop_roots: FxHashMap<Span, LocalId>,
    /// Some body reads a `shared` like its `T` (`Intrinsic::SharedRead`): only then does the
    /// ownership pass track `FnInfo::modified_params`.
    pub frozen_reads: bool,
    /// Gated functions found so far (`body::gated`), for `hir::Program::cpu_targets`.
    pub cpu_targets: FxHashMap<DefId, crate::hir::CpuTarget>,
    /// Dispatch groups for error types (`crate::throws`), built on first use.
    pub groups: Option<crate::throws::Groups>,
    /// Bodies that may hold object literals implementing interfaces (`body::literal_hosts`).
    pub literal_hosts: crate::body::literal_hosts::LiteralHosts,
    /// What the methods of the object literals being checked throw, by interface slot, before
    /// their implementations exist (`body/expr/iface_object.rs`; read by `crate::throws`).
    pub provisional_slots: Vec<(DefId, u32, Vec<crate::defs::ThrowSrc>)>,
    /// The interfaces made from object literals with no expected type
    /// (`body/expr/literal_iface.rs`), whose methods all use `this`.
    pub literal_ifaces: FxHashSet<DefId>,
    /// The structs (or classes) of object literals implementing interfaces
    /// (`body/expr/iface_object.rs`), whose `set k` fields hold the setters.
    pub literal_structs: FxHashSet<DefId>,
    /// The literal interfaces being displayed (`display.rs`): one inside its own members
    /// (`add(): this`) shows as `this`.
    pub literal_display: std::cell::RefCell<Vec<DefId>>,
    /// The adapters converting generic interface values to `unknown` type arguments
    /// (`body/expr/iface_adapt.rs`), made on first use.
    pub iface_adapters: FxHashMap<
        crate::body::expr::iface_adapt::AdapterKey,
        crate::body::expr::iface_adapt::Adapter,
    >,
    /// Checked conversions of dynamic values to generic interfaces (`iface_adapt_dynamic`).
    pub(crate) dyn_iface_targets: Vec<crate::body::expr::iface_adapt_dynamic::DynIfaceTarget>,
    /// Error types committed to while checking bodies, re-checked after inference.
    pub throw_checks: Vec<crate::throws::ThrowCheck>,
    /// Function types written without `throws` and what is stored into them (`throws::fn_types`).
    pub fn_sites: crate::throws::FnSites,
    /// The number of definitions collected from declarations, before any body was checked.
    pub collected_defs: usize,
    /// Generator methods implementing an interface method (`Iterable.[Symbol.iterator]`) with an
    /// inferred error type, which must be `never` (the interface's is fixed; throws/checks.rs).
    pub iface_generators: Vec<(crate::hir::DefId, String)>,
    /// Slot methods of object literals returning the promise of a function value that never
    /// rejects, and that type: made async when their dispatch group's promises may reject
    /// (`collect::forwarders::await_forwarded`).
    pub promise_forwarders: Vec<(crate::hir::DefId, TyId)>,
    /// Items declared inside blocks, visible by name within their block.
    pub nested: Vec<crate::collect::NestedItem>,
    /// Names bound in each nested definition's enclosing functions ("cannot capture" error).
    pub nested_locals: FxHashMap<DefId, Vec<String>>,
    /// For each item nested in a method (or constructor) of a class or struct: that type. Its
    /// body is inside the type's body, for `private` and `#x` (TypeScript).
    pub enclosing_class: FxHashMap<DefId, DefId>,
    /// Static methods using `this`/`super` and their copies: the class `this` is (`static_this`).
    pub static_this: FxHashMap<DefId, DefId>,
    /// The copy of a static method (using `this`) whose `this` is a given subclass.
    pub static_copies: FxHashMap<(DefId, DefId), DefId>,
    /// For each copy of a static method: the method.
    pub static_copy_of: FxHashMap<DefId, DefId>,
    /// Errors in copies of static methods reported only if the copy is used, and the copies
    /// used, each with the function using it (`None`: outside any) (`body::static_copy_uses`).
    pub copy_diags: Vec<(DefId, Diagnostic)>,
    pub copy_uses: Vec<(Option<DefId>, DefId)>,
    /// Interface values converted to the object type of their shape, checked once the bodies are
    /// (`body::expr::iface_from_object::check_unwraps`).
    pub unwraps: Vec<crate::body::expr::iface_from_object::Unwrap>,
    /// Name spans of the nested functions made from local generic arrows (`generic_arrows`).
    pub generic_arrow_fns: FxHashSet<Span>,
    /// Name spans of every function made from a generic arrow (diagnostics show arrows).
    pub generic_arrow_all: FxHashSet<Span>,
    /// Why recursive aliases were not rewritten (`recursive_aliases::Blocked`).
    pub alias_blocked: crate::recursive_aliases::Blocked,
    /// Name spans of capturing nested functions reported by `crate::nested_closures`.
    pub kept_nested: FxHashSet<Span>,
    /// Name spans of the `const`s that closures in their own initializer use (`body::late_consts`):
    /// a closure may capture one before it is assigned.
    pub late_consts: FxHashSet<Span>,
    /// Per generator with a dynamic default (and its type arguments), the function its value
    /// is (`gen_value.rs`).
    pub generator_values: FxHashMap<(DefId, Vec<TyId>), (DefId, TyId)>,
    /// Recursive closure groups made by `crate::nested_closures`.
    pub closure_groups: Vec<crate::nested_closures::ClosureGroup>,
    /// Group and member of each group member, by the name span of its `const`.
    pub group_names: FxHashMap<Span, (usize, usize)>,
    /// Definitions of the closures that are members of a recursive closure group.
    pub group_defs: FxHashSet<DefId>,
    /// TypeScript-form overloads exist (`crate::overloads`): calls choose among `name#k`.
    pub has_overloads: bool,
    /// A user module calls a `throw()` method (`crate::ts_protocol::calls_throw`).
    pub gen_throw: bool,
    /// Names of overloaded methods (`crate::overloads::Rewritten::methods`).
    pub overloaded_methods: FxHashSet<String>,
    /// Each JSX module's runtime, resolved on first use (`None` after its errors).
    pub jsx_providers: FxHashMap<usize, Option<std::rc::Rc<crate::body::expr::jsx::Provider>>>,
    /// JSX component adapters, checked after ownership inference.
    pub jsx_adapters: Vec<crate::body::expr::jsx::Adapter>,
    /// Sync wrappers of async closures (`body/expr/callback.rs`) and the type of the async
    /// closure each one calls: the thread analysis (`ownership::local_async`) matches a wrapper
    /// stored on the heap by that type, the function the user wrote.
    pub callback_wrappers: rustc_hash::FxHashMap<DefId, TyId>,
    /// Captured variables reported as modified by a closure that crosses threads, with the
    /// closure's span: the move analysis doesn't report the follow-on "use of moved value".
    pub reported_captures: Vec<(String, velt_common::Span)>,
    /// Spans of the sync arrows passed to `serve` that are checked as async ones
    /// (`body/expr/callback.rs` `thread_arrow`): diagnostics call them handlers, as written.
    pub sync_handlers: Vec<velt_common::Span>,
    /// The async adapters calling a function value passed to `serve` as its handler
    /// (`body/expr/callback.rs`): the sync closures flowing into them are handlers.
    pub thread_adapters: Vec<DefId>,
    /// Side tables for [`crate::ide`] (`None` when compiling).
    pub ide: Option<Box<crate::ide::record::Recorder>>,
    /// Memoized `Ctx::is_shared_value` answers (asked for every local of every body).
    pub shared_memo: FxHashMap<TyId, bool>,
    /// Function values kept by functions with fixed modes (`crate::ownership::KeptFns`).
    pub kept_fns: crate::ownership::KeptFns,
    /// Memoized purity of functions that initialize module constants (`body::pure_init`).
    pub pure_fns: crate::body::pure_init::PurityMemo,
    /// Module constants kept in the module state because of how the program uses them, in the
    /// order `crate::kept_consts::keep` kept them (a rolled-back check undoes its own).
    pub kept_consts: Vec<DefId>,
    /// The classes extending each class, directly or not (`ownership::locked::regions`), built
    /// on first use once every class is known.
    pub subclasses: Option<FxHashMap<DefId, Vec<DefId>>>,
    /// The member names symbols give (`crate::symbols`).
    pub symbol_keys: crate::symbols::KeyNames,
    /// Set while [`Ctx::match_context`] runs (`crate::infer`).
    pub matching_context: bool,
    /// Bodies being checked, outermost first (inferring one on it is recursion: `body::returns`).
    pub checking: Vec<DefId>,
    /// Signature comparisons waiting for inferred result types (`body::returns`).
    pub ret_checks: Vec<crate::defs::RetCheck>,
    /// Uses of functions whose return types are being inferred (`body::recursion`).
    pub rec: crate::body::recursion::RecState,
    /// Methods some subclass overrides (`collect::ret_infer`): a result inferred from JS-number
    /// integers is a `number` (`f64`) there, which an override may return a fraction in.
    pub overridden: FxHashSet<DefId>,
    /// The address of a local near the bottom of the checking thread's stack, and how much of
    /// the stack inferring return types may use above it (`body::returns::ret_of`).
    pub stack_base: usize,
    pub stack_budget: usize,
    /// Closures held in a `const` and only called, and `const me = this`, may borrow instead of
    /// sharing (`crate::ownership::demote_local_closures`, `body::const_borrow`). Off for the
    /// second check of a program the borrowing version rejects (`crate::check_with`).
    pub held_borrows: bool,
    /// Per module: namespace members visible only inside their namespace (`crate::namespaces`).
    pub ns_private: Vec<crate::namespaces::Private>,
    /// Some closure or `const me = this` was made to borrow.
    pub held_borrows_used: bool,
    /// Module constants taken over, reported: (constant, start of the value taken) → index in
    /// `diags` (`body::moved_const`).
    pub moved_consts: FxHashMap<(crate::hir::DefId, velt_common::Span), usize>,
    /// The passes from `demote_local_closures` on reported an error (`crate::retry_sharing`).
    pub borrow_pass_errors: bool,
    /// Widened call results whose callees must return fresh values (`crate::fresh_returns`).
    pub fresh_checks: Vec<crate::fresh_returns::FreshCheck>,
    /// Objects converted by copying their fields, and telling writes (`crate::object_copies`).
    pub object_copies: crate::object_copies::Copies,
    /// The spans of the `super.m(args)` calls (`hir::Program::super_calls`).
    pub super_calls: rustc_hash::FxHashSet<velt_common::Span>,
    /// Resolved type-parameter defaults (`crate::type_defaults`).
    pub type_defaults: crate::type_defaults::TypeDefaults,
    /// Protocol types' second arguments written before base classes were known (`ts_protocol`).
    pub deferred_ts_returns: Vec<crate::ts_protocol::TsReturnCheck>,
    pub diags: Diagnostics,
    /// Per body being checked (innermost last): what the bodies it checked meanwhile added,
    /// and closures to create again (`body::recheck`).
    pub rechecks: Vec<crate::body::recheck::Frame>,
    /// Closures created by body checks, in order (`body::recheck`).
    pub closure_defs: Vec<DefId>,
    /// The modules of the JS API (`body::expr::numbers::is_js_api`): each prelude module
    /// (`None`: all of it), and each module the prelude's globals re-export from, with the
    /// names re-exported from it (`Some`).
    pub js_api_modules: std::cell::OnceCell<FxHashMap<usize, Option<FxHashSet<String>>>>,
    /// Integer types decided for locals declared from literals, by function and declaration
    /// span (`body::literal_locals`).
    pub literal_types: FxHashMap<DefId, FxHashMap<Span, TyId>>,
    /// What the last check of a body found about those locals.
    pub literal_found: FxHashMap<DefId, crate::body::literal_locals::LiteralLocals>,
    /// Modules whose parse reported errors (`velt check` goes on with the recovered syntax
    /// trees, [`crate::check_recovered`]), with the words of their source: a name missing from
    /// one may be a declaration the parser dropped (`collect::exports::maybe_dropped`).
    pub recovered: FxHashMap<usize, FxHashSet<String>>,
    /// Names bound by such an import that found nothing (file of the importing module, local
    /// name): a use of one reports nothing more, as its import didn't (`Ctx::not_found`).
    pub poisoned: FxHashSet<(velt_common::FileId, String)>,
    /// What each module exports, computed once (`crate::collect::exports`).
    pub export_memo: std::cell::RefCell<crate::collect::exports::Memo>,
    /// For each module, the module each of its import specifiers resolved to
    /// (`exports::import_target`).
    pub import_targets: Vec<FxHashMap<&'m str, usize>>,
}

impl<'m> Ctx<'m> {
    pub fn new(modules: &'m [SourceModule], root: usize) -> Self {
        Ctx {
            modules,
            root,
            export_memo: Default::default(),
            import_targets: crate::collect::exports::import_targets(modules),
            ty: Types::new(),
            defs: vec![],
            info: vec![],
            def_spans: vec![],
            scopes: modules
                .iter()
                .map(|m| ModuleScope {
                    is_std: m.is_std,
                    ..Default::default()
                })
                .collect(),
            prelude: FxHashMap::default(),
            aliases: vec![],
            extensions: vec![],
            impls: vec![],
            iface_writes: Default::default(),
            part_ifaces: Default::default(),
            synth_ifaces: Default::default(),
            union_views: Default::default(),
            any: None,
            impl_index: Default::default(),
            ext_impls: FxHashMap::default(),
            ext_by_iface: FxHashMap::default(),
            ext_memo: Default::default(),
            ext_exists_memo: Default::default(),
            ext_memo_impls: 0,
            in_blanket: false,
            impl_scope: vec![],
            anon: FxHashMap::default(),
            readonly_twins: FxHashMap::default(),
            any_unknown_views: false,
            objects: Default::default(),
            brands: FxHashMap::default(),
            brand_keys: FxHashMap::default(),
            generic_brands: FxHashSet::default(),
            record_views: FxHashMap::default(),
            field_only: FxHashMap::default(),
            field_only_of: FxHashMap::default(),
            shapes_done: false,
            query_locals: vec![],
            bodies_ok: false,
            sigq: Default::default(),
            checking_unused_aliases: false,
            primitive_guards: false,
            type_level: Default::default(),
            writes_undefined: false,
            shaped: FxHashSet::default(),
            shaping: vec![],
            laid_out: FxHashSet::default(),
            canon_memo: FxHashMap::default(),
            counted_objects: Vec::new(),
            replaced_unions: FxHashSet::default(),
            module_slots: Vec::new(),
            module_scripts: Vec::new(),
            promoted_consts: FxHashSet::default(),
            global_props: Default::default(),
            global_cycles: 0,
            unions: FxHashMap::default(),
            recursive_unions: FxHashMap::default(),
            alias_names: FxHashMap::default(),
            generic_overrides: vec![],
            getter_only_setters: FxHashSet::default(),
            display_params: vec![],
            defaults_checked: FxHashSet::default(),
            classes_below: FxHashMap::default(),
            fills: FxHashMap::default(),
            default_throws: FxHashMap::default(),
            object_impls: Default::default(),
            hook_adapters: FxHashMap::default(),
            field_defaults_checked: FxHashSet::default(),
            closure_counts: FxHashMap::default(),
            fn_defs: vec![],
            fn_values: vec![],
            frozen_elems: FxHashSet::default(),
            loop_roots: FxHashMap::default(),
            frozen_reads: false,
            cpu_targets: FxHashMap::default(),
            groups: None,
            literal_hosts: Default::default(),
            provisional_slots: vec![],
            literal_ifaces: Default::default(),
            literal_structs: Default::default(),
            literal_display: Default::default(),
            iface_adapters: FxHashMap::default(),
            dyn_iface_targets: vec![],
            throw_checks: vec![],
            fn_sites: Default::default(),
            collected_defs: 0,
            iface_generators: vec![],
            promise_forwarders: vec![],
            nested: vec![],
            nested_locals: FxHashMap::default(),
            enclosing_class: FxHashMap::default(),
            static_this: FxHashMap::default(),
            static_copies: FxHashMap::default(),
            static_copy_of: FxHashMap::default(),
            copy_diags: vec![],
            copy_uses: vec![],
            unwraps: vec![],
            generic_arrow_fns: FxHashSet::default(),
            kept_nested: FxHashSet::default(),
            late_consts: FxHashSet::default(),
            generator_values: FxHashMap::default(),
            closure_groups: vec![],
            group_names: FxHashMap::default(),
            group_defs: FxHashSet::default(),
            generic_arrow_all: FxHashSet::default(),
            alias_blocked: Default::default(),
            has_overloads: false,
            gen_throw: false,
            overloaded_methods: FxHashSet::default(),
            jsx_providers: FxHashMap::default(),
            jsx_adapters: vec![],
            callback_wrappers: Default::default(),
            reported_captures: vec![],
            sync_handlers: vec![],
            thread_adapters: vec![],
            ide: None,
            recovered: FxHashMap::default(),
            poisoned: FxHashSet::default(),
            shared_memo: FxHashMap::default(),
            kept_fns: Default::default(),
            pure_fns: FxHashMap::default(),
            kept_consts: vec![],
            subclasses: None,
            symbol_keys: Default::default(),
            matching_context: false,
            checking: vec![],
            ret_checks: vec![],
            rec: Default::default(),
            overridden: FxHashSet::default(),
            stack_base: crate::stack_address(),
            stack_budget: crate::SEMA_STACK_BUDGET,
            held_borrows: true,
            ns_private: vec![],
            held_borrows_used: false,
            moved_consts: Default::default(),
            borrow_pass_errors: false,
            fresh_checks: vec![],
            object_copies: Default::default(),
            super_calls: Default::default(),
            type_defaults: Default::default(),
            deferred_ts_returns: vec![],
            diags: vec![],
            rechecks: vec![],
            closure_defs: vec![],
            js_api_modules: std::cell::OnceCell::new(),
            literal_types: FxHashMap::default(),
            literal_found: FxHashMap::default(),
        }
    }

    pub fn error(&mut self, d: Diagnostic) {
        self.diags.push(d);
    }

    pub fn err(&mut self, msg: impl Into<String>, span: Span) {
        self.diags.push(Diagnostic::error(msg, span));
    }

    /// `msg` for a name `name` that nothing declares: with a note naming the import when it is
    /// one of the SIMD types, which are not global (`velt:simd`).
    pub fn not_found(&mut self, msg: String, name: &str, span: Span) {
        if !self.poisoned.is_empty() && self.poisoned.contains(&(span.file, name.to_string())) {
            return;
        }
        let mut d = Diagnostic::error(msg, span);
        if crate::hir::SimdTy::named(name).is_some() {
            d = d.with_note(format!(
                "import it: `import {{ {name} }} from \"velt:simd\";`"
            ));
        }
        self.diags.push(d);
    }

    /// Define the closure of a rolled-back check again as `info` (`body::recheck`).
    pub(crate) fn redefine_closure(&mut self, id: DefId, span: Span, info: DefInfo<'m>) {
        self.fn_defs.push(id);
        self.defs[id.0 as usize] = None;
        self.info[id.0 as usize] = info;
        self.def_spans[id.0 as usize] = span;
    }

    pub fn alloc_def(&mut self, span: Span, info: DefInfo<'m>) -> DefId {
        let id = DefId(self.defs.len() as u32);
        if matches!(info, DefInfo::Fn(_)) {
            self.fn_defs.push(id);
        }
        self.defs.push(None);
        self.info.push(info);
        self.def_spans.push(span);
        id
    }

    pub fn fn_info(&self, d: DefId) -> &FnInfo<'m> {
        match &self.info[d.0 as usize] {
            DefInfo::Fn(f) => f,
            _ => panic!("ICE: def {d:?} is not a function"),
        }
    }

    pub fn fn_info_mut(&mut self, d: DefId) -> &mut FnInfo<'m> {
        match &mut self.info[d.0 as usize] {
            DefInfo::Fn(f) => f,
            _ => panic!("ICE: def {d:?} is not a function"),
        }
    }

    pub fn try_fn(&self, d: DefId) -> Option<&FnInfo<'m>> {
        match &self.info[d.0 as usize] {
            DefInfo::Fn(f) => Some(f),
            _ => None,
        }
    }

    pub fn adt(&self, d: DefId) -> Option<&AdtInfo<'m>> {
        match &self.info[d.0 as usize] {
            DefInfo::Adt(a) => Some(a),
            _ => None,
        }
    }

    pub fn adt_mut(&mut self, d: DefId) -> &mut AdtInfo<'m> {
        match &mut self.info[d.0 as usize] {
            DefInfo::Adt(a) => a,
            _ => panic!("ICE: def {d:?} is not an ADT"),
        }
    }

    pub fn enum_info(&self, d: DefId) -> Option<&EnumInfo<'m>> {
        match &self.info[d.0 as usize] {
            DefInfo::Enum(e) => Some(e),
            _ => None,
        }
    }

    pub fn iface(&self, d: DefId) -> Option<&IfaceInfo<'m>> {
        match &self.info[d.0 as usize] {
            DefInfo::Iface(i) => Some(i),
            _ => None,
        }
    }

    pub fn global(&self, d: DefId) -> Option<&GlobalInfo<'m>> {
        match &self.info[d.0 as usize] {
            DefInfo::Global(g) => Some(g),
            _ => None,
        }
    }

    /// Is `t` a class instance type? Returns (class def, type args).
    pub fn class_of(&self, t: TyId) -> Option<(DefId, Vec<TyId>)> {
        match self.ty.kind(t) {
            TyKind::Adt(d, args) if self.adt(*d).is_some_and(|a| a.kind == AdtKind::Class) => {
                Some((*d, args.clone()))
            }
            _ => None,
        }
    }

    /// Base class type of class instance type `t` (with `t`'s type args substituted).
    pub fn base_of(&mut self, t: TyId) -> Option<TyId> {
        let (d, args) = self.class_of(t)?;
        let base = self.adt(d)?.base?;
        Some(self.subst(base, &args))
    }

    /// Is class `sub` class `sup` or one of its (transitive) subclasses?
    pub fn class_extends(&self, sub: DefId, sup: DefId) -> bool {
        let mut cur = Some(sub);
        for _ in 0..64 {
            match cur {
                Some(d) if d == sup => return true,
                Some(d) => {
                    cur = self
                        .adt(d)
                        .and_then(|a| a.base)
                        .and_then(|b| self.class_of(b))
                        .map(|(d, _)| d)
                }
                None => return false,
            }
        }
        false
    }

    /// Name lookup at module level: own items and imports, then the prelude.
    pub fn lookup_item(&self, module: usize, name: &str) -> Option<Item> {
        self.scopes[module]
            .items
            .get(name)
            .or_else(|| self.prelude.get(name))
            .copied()
    }

    /// Name lookup at a source position of `module`: the innermost nested item whose block
    /// contains `at`, then module level.
    pub fn lookup_item_at(&self, module: usize, name: &str, at: Span) -> Option<Item> {
        let file = self.modules[module].file;
        let nested = self
            .nested
            .iter()
            .filter(|n| n.module == module && n.name == name && at.file == file)
            .filter(|n| n.lo <= at.lo && at.hi <= n.hi)
            .min_by_key(|n| n.hi - n.lo);
        match nested {
            Some(n) => Some(n.item),
            None => self.lookup_item(module, name),
        }
    }

    /// A string or `string | null`: a value whose copy is a refcount increment at most, so moves
    /// of it are soft (`crate::ownership::strings`).
    pub fn is_string_value(&self, t: TyId) -> bool {
        t == self.ty.str_ || self.ty.opt_payload(t) == Some(self.ty.str_)
    }

    /// Semantics stage 2: a non-Copy value that is *shared* — another reference to the same
    /// value — where its place stays in use or cannot be moved from, instead of being moved
    /// (strings, objects, arrays, maps, closures, promises: another holder of the same
    /// promise, #212): every type that is not Copy.
    pub fn is_shared_value(&mut self, t: TyId) -> bool {
        if let Some(&b) = self.shared_memo.get(&t) {
            return b;
        }
        let b = !self.is_copy(t);
        self.shared_memo.insert(t, b);
        b
    }

    /// Fully qualified name of a module-level item.
    pub fn qualify(&self, module: usize, name: &str) -> String {
        if module == self.root {
            name.to_string()
        } else {
            format!("{}::{}", self.modules[module].path, name)
        }
    }
}
