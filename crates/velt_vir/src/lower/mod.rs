//! HIR → VIR lowering. See the crate docs (lib.rs) for the ABI and layout conventions.
//!
//! - [`Cx`] (program.rs) holds program-level state: the monomorphic type table (types.rs),
//!   aggregate layouts (layout.rs), extern/static interning (vtables are relocated static
//!   tables, glue/vtable.rs) and the worklist of functions to build, keyed by [`Work`] (user
//!   function instances by `(DefId, type args)`, per-type glue, borrow-ABI thunks, async poll/drop
//!   functions and small helpers).
//! - [`FnLower`] builds one VIR function. For user functions abi.rs sets up params/locals and
//!   stmt.rs, expr.rs, place.rs, call.rs, … lower the body; glue/ builds compiler-generated
//!   functions with the same CFG primitives (cfg.rs). A stack of [`Scope`]s owns drop
//!   obligations, loop targets, `try` handlers and `finally` blocks, so every exit edge emits
//!   exactly the cleanup it needs (drops.rs, errors.rs).
//! - Drop elaboration: a droppable local whose ownership state changes inside a conditional region
//!   (relative to its declaration) gets a Bool drop flag (flags.rs); all others are tracked
//!   statically during lowering.
//! - Async functions (M3) become poll/drop state machines (async_fn/); JSON glue lives in json/.
//! - Source locations (srcloc.rs): with a source map every statement records the location of
//!   the HIR expression/statement being lowered, and panics name their location
//!   (track_caller.rs for standard-library helpers such as `unwrap`).

mod abi;
mod adapters;
mod adt;
mod any;
mod any_args;
mod any_array_proto;
mod any_borrowed;
mod any_builtin;
mod any_checks;
mod any_delete;
mod any_eq;
mod any_errors;
mod any_exact;
mod any_fn_props;
mod any_generic_fn;
mod any_get_builtin;
mod any_glue;
mod any_has;
mod any_has_builtin;
mod any_iface;
mod any_keys;
mod any_member;
mod any_not_fn;
mod any_number;
mod any_object_fill;
mod any_proxy;
mod any_record;
mod any_reflect;
mod any_rest;
mod any_set;
mod any_set_site;
mod any_to_record;
mod any_tuple;
mod any_unbox_slow;
mod any_view;
mod append;
mod array;
mod array_view;
mod array_view_index;
mod array_view_ops;
mod assign_target;
mod async_fn;
mod atomic;
mod atomic_ref;
mod atomic_str;
mod attempt;
mod bits;
mod boxes;
mod boxing;
mod kept;
mod passes;
pub use boxing::CountedSeed;
pub use kept::{Delta, KeptLowering, Refused};
use passes::lower_program_kept;
pub(crate) use passes::{lower_program, lower_program_seeded};
mod c_call;
mod c_export;
pub(crate) mod c_global;
mod call;
mod callee;
mod caller_aliases;
mod caller_stores;
mod cells;
mod cfg;
mod class_test;
mod cleanup;
mod closure;
mod console;
mod cpu_checks;
mod ctor_init;
mod debug_vars;
mod decorators;
mod deferred_versions;
mod dispatch;
mod drops;
mod entry;
mod epoch_sections;
mod errors;
mod expr;
mod ext_exists;
mod flags;
mod for_of;
mod for_of_shared;
mod foreign;
mod frame_envs;
mod lower_profile;
mod raw_memory;
mod sched;
mod slot;
mod state;
use state::{DropEntry, LInfo, LState, Scope, ScopeKind};
mod fn_shapes;
mod fresh_fns;
mod frozen;
mod glue;
mod half;
mod hand_result;
pub(crate) mod hook_calls;
mod hooks;
mod host;
mod impl_lookup;
mod index_order;
mod intrinsics;
mod js_string;
mod json;
mod key_any;
mod keys;
mod kinds;
mod layout;
mod leave_levels;
mod literal_accessors;
mod literal_moves;
mod match_switch;
mod matching;
mod module_state;
mod narrowed_paths;
mod operand;
mod ops;
mod option_text;
mod param_any;
mod param_attrs;
mod pattern;
mod place;
mod program;
mod proxy_call;
mod proxy_method;
mod proxy_throws;
mod proxy_typed;
mod proxy_write;
mod quiet;
mod rc;
mod receiver;
mod reg_args;
mod replaceable;
mod rt;
mod same;
mod sequence;
mod shape;
mod share;
mod simd;
mod spread_object;
mod srcloc;
mod stabilize;
mod static_atomic;
mod stmt;
mod str_data;
mod strbuf;
mod stringify;
mod strings;
mod symbols;
mod system;
mod system_views;
mod template;
mod this_fn;
mod thread_local;
mod track_caller;
mod transfer;
mod truthy;
mod type_tests;
mod types;
mod upcast;
mod view_options;
mod view_unswitch;
mod view_unswitch_addrs;
mod view_unswitch_handover;
mod view_unswitch_plain;
mod weak;
mod weak_object;
mod weak_view;
mod widen;
mod work;
mod write_effects;

use rustc_hash::{FxHashMap, FxHashSet};
use std::collections::VecDeque;
use std::rc::Rc;

pub(super) use kinds::LkMode;
use kinds::ThunkKind;
use velt_sema::hir::{self, DefId, LocalId, TyId, TyTable};
use work::Work;

use crate::vir::{
    self, AggLayout, BlockId, Const, ExternFn, ExternId, FuncId, Function, Local, LocalDecl,
    Operand, Place, SrcLoc, StaticData, StaticId, Terminator, Ty,
};
use crate::LowerOptions;

pub(crate) use cfg::successors;
pub(crate) use glue::Glue;
// The VIR interpreter (tests) formats like the runtime.
use glue::VtableKey;
#[cfg(test)]
pub(crate) use glue::{inspect_key, inspect_quote};

#[cfg(test)]
pub(crate) use lower_profile::PASSES;

/// Internal-compiler-error: lowering was handed something sema should never produce.
fn ice(msg: impl std::fmt::Display) -> ! {
    panic!("ICE: {msg}")
}

fn unit() -> Operand {
    Operand::Const(Const::Unit, Ty::Unit)
}

fn cint(v: i128, ty: Ty) -> Operand {
    Operand::Const(Const::Int(v), ty)
}

fn cfunc(f: FuncId) -> Operand {
    Operand::Const(Const::Func(f), Ty::Ptr)
}

/// Program-level lowering state.
struct Cx<'h> {
    hir: &'h hir::Program,
    /// The prelude's `Proxy` class type, once looked up (proxy_throws.rs).
    prelude_proxy: Option<Option<TyId>>,
    /// Native library inits `velt_main` runs first (entry.rs).
    native_inits: Vec<crate::NativeInit>,
    /// The prelude's `Proxy.__view` (any_view.rs), once looked up.
    view_fn: Option<Option<DefId>>,
    /// The standard library's functions by qualified name, built on the first lookup of the
    /// JSON glue (json/reader.rs `json_std_fn`): one map, not a scan of every def per call.
    std_fns: std::cell::OnceCell<FxHashMap<&'h str, DefId>>,
    /// The program pins a task (`spawn(…, { worker })`): `velt_main` selects the per-core
    /// scheduler. Known once every function but `velt_main` is lowered, hence built last.
    uses_placement: bool,
    /// Report every use of a captured variable's cell to the debug runtime (cells.rs,
    /// `LowerOptions::cell_checks`).
    cell_checks: bool,
    /// The target triple, for system and host functions (`LowerOptions::target`).
    target: String,
    /// CPU checks known while lowering (cpu_checks.rs).
    cpu_checks: cpu_checks::CpuChecks,
    /// Calls of system and host functions the target lacks: (the declaration, the calling function, the
    /// user code being lowered then, `user_site`).
    unavailable: Vec<(DefId, Option<DefId>, velt_common::Span)>,
    /// Deferred types an instantiation met that sema didn't reduce (`types.rs` `reduce_op`).
    unreduced: Vec<TyId>,
    /// C functions called with a signature their symbol already has another one of (c_call.rs).
    c_clashes: Vec<velt_common::Diagnostic>,
    /// The symbols C functions declared (c_call.rs): a later extern of the same symbol with
    /// another signature is that clash, reported, not an internal error.
    c_symbols: FxHashMap<String, c_call::CDecl>,
    /// The statics of C variables (c_global.rs), by link symbol.
    c_globals: FxHashMap<String, StaticId>,
    /// `LowerOptions::jit`: C variables by their own symbols (c_global.rs).
    jit: bool,
    /// The C declarations whose signature clash is reported (once each).
    c_clashes_reported: rustc_hash::FxHashSet<DefId>,
    /// velt/ffi's hooks found so far (c_call.rs): each is looked up once per program.
    ffi_hooks: FxHashMap<&'static str, DefId>,
    /// Per user call site of a standard-library function, that function (call.rs): what an
    /// unavailable system function deep in std is reported as, at the user's call.
    std_calls: FxHashMap<velt_common::Span, DefId>,
    /// The aggregates of C structs (system.rs), by the table's struct.
    system_aggs: FxHashMap<usize, vir::AggId>,
    /// The 16-byte slot a `struct` returned in a register passes through (abi.rs `RegRet`).
    reg_slot_agg: Option<vir::AggId>,
    /// Lower no `await` steps (async_fn/await_order.rs): the program was lowered once and
    /// nothing in it can make a task's await word non-0.
    no_await_steps: bool,
    /// This pass only finds the counted types (`lower_program`): functions leave out the work
    /// that finds none, the versions of their view loops (view_unswitch.rs).
    facts_only: bool,
    /// The functions of this facts-only pass whose view loops are left to version once the
    /// counted types are complete, and those of the functions being built (deferred_versions.rs).
    deferred: FxHashMap<FuncId, deferred_versions::Deferred>,
    deferred_new: Vec<deferred_versions::Deferred>,
    /// The functions whose view loops this pass versioned (`VELT_LOWER_PROFILE`).
    versioned: usize,
    /// Copy of the HIR type table, extended with substituted (monomorphic) types.
    types: TyTable,
    aggs: Vec<AggLayout>,
    externs: Vec<ExternFn>,
    extern_map: FxHashMap<String, ExternId>,
    /// The tuple types the function being built writes in place, and those of its `Copy`
    /// parameters it only reads (boxing/tuples.rs).
    cur_tuple_writes: Vec<TyId>,
    cur_read_params: Vec<TyId>,
    statics: Vec<StaticData>,
    /// The module state's descriptor, once a slot is used (module_state.rs).
    module_desc: Option<StaticId>,
    /// The module state slots some code assigns as a whole, once asked (module_state.rs).
    reassigned_slots: Option<FxHashSet<hir::DefId>>,
    /// The fields of object types some code assigns, once asked (index_order.rs).
    assigned_fields: Option<index_order::AssignedFields>,
    /// What each function may store, with what it calls, once asked (write_effects.rs).
    fn_writes: Option<write_effects::FnWrites>,
    /// The prelude's `Error` class, once looked up (js_string.rs).
    prelude_error: Option<Option<hir::DefId>>,
    /// The prelude's `TypeError` class, once looked up (any_not_fn.rs).
    prelude_type_error: Option<Option<hir::DefId>>,
    /// The prelude's top-level `await` mark and what it forwards to (async_fn/suspend.rs).
    tla_marks: Option<[Option<hir::DefId>; 2]>,
    static_map: FxHashMap<(Vec<u8>, u32), StaticId>,
    /// Each module constant table's stored elements (`table_read`): data and length, else `None`.
    tables: FxHashMap<hir::DefId, Option<(StaticId, usize)>>,
    /// Each static atomic's data so far, `None` for a global that is not one (static_atomic.rs).
    static_atomics: FxHashMap<hir::DefId, Option<StaticId>>,
    /// Each module-level `let`'s slot in the module state (its position in `ModuleState::slots`).
    slot_index: FxHashMap<hir::DefId, u32>,
    fut_header: Option<crate::vir::AggId>, // a heap future's header (raw_memory.rs `fut_header`)
    sched_tasks: bool, // the executor in Velt's hooks start promises or spawn tasks (sched.rs)
    sched_used: bool,  // a task or timer hook of the executor in Velt is called (module_state.rs)
    held: Option<Vec<(FuncId, Work)>>, // module state's accesses, until main is built (drain)
    statics_table: Option<StaticId>, // the process statics' table, its release to fill in
    /// The class id number of each `struct` type with an interface table (class_test.rs
    /// `VALUE_IDS`), in the order the tables are made.
    value_ids: FxHashMap<TyId, u64>,
    /// Slots are filled as the worklist is processed.
    funcs: Vec<Option<Function>>,
    work_map: FxHashMap<Work, FuncId>,
    queue: VecDeque<(FuncId, Work)>,
    /// The glues that depend on the types `any` values hold (`AnyIface`, any_iface.rs, and
    /// `AnyNumeric`, any_number.rs), built last, each with the number of such types then.
    iface_glues: Vec<(FuncId, Work, Option<usize>)>,
    /// The glues `length` and `name` of function values go through (fn_shapes.rs).
    fn_tables: Vec<(FuncId, Work)>,
    /// Params given `noalias` or `frozen`, with their types, for `unfreeze_module_reach`
    /// (boxing/module_reach.rs); those of the function being declared until it is built.
    unique_params: Vec<(FuncId, u32, TyId)>,
    declared_unique: Vec<(u32, TyId)>,
    /// Functions currently being built (eager builds of poll functions detect recursion).
    building: FxHashSet<FuncId>,
    /// The functions built since it was set (kept.rs: what a splice lowered).
    built_log: Option<Vec<FuncId>>,
    /// Layout facts of every async function instance whose poll function has been built.
    asyncs: FxHashMap<(DefId, Vec<TyId>), async_fn::AsyncInfo>,
    /// Closure instances whose declared scalar params are passed by pointer: `Mutex.with`
    /// callbacks update the locked value in place.
    by_ref_params: FxHashSet<(DefId, Vec<TyId>)>,
    lay: layout::Layouts,
    /// Class tables and the glue slots code dispatches (glue/slot_uses.rs).
    glue_slots: glue::GlueSlots,
    /// Line tables when lowering with a source map (`lower_with`).
    locs: Option<srcloc::LocMap>,
    /// Debug types of source variables (`LowerOptions::debug_info`, debug_vars.rs).
    debug: Option<debug_vars::DebugTypes>,
    /// Memoized `tracks_caller` answers.
    tracked: FxHashMap<DefId, bool>,
    /// HIR nodes of the standard-library functions `tracks_caller` scanned (track_caller.rs).
    tracked_size: FxHashMap<DefId, usize>,
    /// Memoized `quiet` answers, per level (quiet.rs).
    quiet: FxHashMap<(DefId, quiet::Quiet), bool>,
    /// The fields written after their object was made, once asked (replaceable.rs).
    field_writes: Option<std::rc::Rc<replaceable::FieldWrites>>,
    /// The params that may reach a function value, once asked: the HIR doesn't change between
    /// passes, so a pass hands them on.
    value_params: Option<std::rc::Rc<velt_sema::value_params::ValueParams>>,
    /// Whether dropping a value of each type can run a `dispose` (replaceable.rs).
    dispose_types: FxHashMap<TyId, bool>,
    /// What direct callees write, once asked (replaceable.rs).
    callee_stores: FxHashMap<u32, Option<std::rc::Rc<replaceable::Stores>>>,
    /// Direct callees asked about, by `replaceable::CalleeId`.
    callees: Vec<(DefId, Vec<TyId>)>,
    /// Their ids, by definition and type arguments.
    callee_ids: FxHashMap<DefId, Vec<(Vec<TyId>, replaceable::CalleeId)>>,
    /// Interned static `VeltStr` objects (`static_str_object`).
    str_objects: FxHashMap<String, StaticId>,
    /// The records of the compiler's symbols (`Intrinsic::SymbolStatic`, symbols.rs).
    symbol_records: symbols::SymbolRecords,
    /// Types whose values carry a reference count in this pass (boxing/).
    boxing: boxing::Boxing,
    /// What [`Cx::number_of`] numbered so far.
    numbered: rustc_hash::FxHashMap<(program::Numbered, u32), u32>,
    /// Sharing facts observed in this pass (boxing/).
    facts: boxing::Facts,
    /// Generic instances shared by shape in this pass (shape/).
    shapes: shape::Shapes,
    /// `Program::impls` indexes per interface (`impls_of`), built on first use.
    iface_impls: Option<FxHashMap<DefId, Rc<[u32]>>>,
    /// Finished `extend` lookups by `(interface, arguments, type)` (impl_lookup.rs), and bound
    /// checks through blocks (ext_exists.rs).
    ext_memo: FxHashMap<impl_lookup::MemoKey, Option<impl_lookup::Fit>>,
    exists_memo: FxHashMap<ext_exists::ExistsKey, bool>,
    /// Classes with a `clone()` of their own, and that method (`own_clone`, transfer.rs),
    /// found on first use.
    own_clones: Option<FxHashMap<DefId, DefId>>,
    /// Memoized `own_clone` answers per class type (only resource owners' are honoured).
    honoured_clones: FxHashMap<TyId, Option<DefId>>,
    /// Memoized `dyn_modes` per (interface, slot).
    dyn_modes_memo: FxHashMap<(DefId, u32), Option<Vec<hir::PassMode>>>,
    /// One type per anonymous object shape (types.rs `canon`).
    anon: types::AnonShapes,
    /// `hir::Program::generated` by work items' canonical type arguments (made on first use).
    generated_canon: Option<FxHashMap<(DefId, Vec<TyId>), DefId>>,
    /// The function whose instance is being built (for `nothing_clashes`).
    cur_def: Option<DefId>,
    /// The read mode of the function being built (`Work::Checked`, `Work::Read`).
    cur_mode: LkMode,
    /// Whether the function being built is a `Work::ReadOwn`, and one in a `Work::Deferred`.
    cur_own: bool,
    cur_own_deep: bool,
    /// Each closure literal's function, and whether it is a `read` callback in read mode.
    closure_parents: FxHashMap<DefId, (Option<DefId>, bool)>,
    /// Functions whose instances made an option of an option with another nothing (`T | null`
    /// at `T = U | undefined`, or the reverse; types.rs `intern`), which this phase cannot
    /// represent (issue #930): reported, not lowered silently.
    nothing_clashes: Vec<Option<DefId>>,
    /// Deep copies of frozen values holding an `Atomic` (atomic.rs `note_frozen_copy`).
    atomic_copies: Vec<velt_common::Span>,
    /// The user code being lowered (atomic.rs `user_span`), for errors found in a standard
    /// library instance: the innermost expression of a user file, or else the user code that
    /// first requested the function being built.
    user_site: velt_common::Span,
    /// Per function, the `user_site` that first requested it.
    fn_sites: rustc_hash::FxHashMap<FuncId, velt_common::Span>,
    /// The first `user_site` that shared each type, or borrowed it through a container
    /// (boxing `note_share`, `note_projection`): where a counted `Atomic` is reported.
    share_sites: rustc_hash::FxHashMap<TyId, velt_common::Span>,
    /// The locks the program shares by value (`counted_lock_diagnostics`).
    shared_locks: rustc_hash::FxHashSet<TyId>,
    /// The shared type each type `close_boxing` reached through shared values came from.
    share_from: rustc_hash::FxHashMap<TyId, TyId>,
    /// Non-zero while a prelude `Map`'s field types are substituted (`keeps_nothings_apart`).
    map_fields: u32,
    /// Whether calls get cleanup pads, and the last poll function's pad links (cleanup.rs).
    unwinding: cleanup::Unwinding,
}

/// Per-function lowering state.
struct FnLower<'c, 'h> {
    cx: &'c mut Cx<'h>,
    /// Type arguments substituted for `TyKind::Param` in the HIR being lowered.
    targs: Vec<TyId>,
    /// Building a glue function: its comparisons are counted where it is called (fresh_fns.rs).
    glue: bool,
    /// Generating the many-threads check's strict walk (glue/many.rs).
    many_strict: bool,
    locals: Vec<LocalDecl>,
    blocks: Vec<(Vec<vir::Stmt>, Option<Terminator>)>,
    /// The locals that hold an `f16`/`bf16` value (its bits), recorded in debug builds of the
    /// compiler for the check that no arithmetic, comparison or switch reads them (`half.rs`).
    half_locals: rustc_hash::FxHashSet<u32>,
    /// Source locations parallel to `blocks`: per statement, and of the terminator.
    block_locs: Vec<(Vec<Option<SrcLoc>>, Option<SrcLoc>)>,
    /// Location recorded for statements emitted now (srcloc.rs).
    loc: Option<SrcLoc>,
    /// In a caller-tracking instance: the call site its panics report (`srcloc::ARG_LOC`: the
    /// one `caller_arg` points to).
    caller_loc: Option<SrcLoc>,
    /// In a caller-tracking instance that takes its caller's location as a parameter: that
    /// parameter, a pointer to the `Str` ` at <path>:<line>:<col>` (empty when unknown).
    caller_arg: Option<vir::Local>,
    /// A block is live once a live block jumps to it; dead blocks are pruned in `finish`.
    live: Vec<bool>,
    cur: BlockId,
    info: Vec<LInfo>,
    scopes: Vec<Scope>,
    out_ptr: Option<Local>,
    /// A `struct` result returned in a register (abi.rs `RegRet`): the slot `out_ptr` points
    /// into, whose `V128` the `return` hands back.
    reg_slot: Option<Local>,
    /// Concrete return type and thrown type of the function being lowered.
    ret_ty: Option<TyId>,
    throws: Option<TyId>,
    /// Shared `panic("division by zero")` blocks per panic location, built in `finish` if
    /// anything jumps to them.
    div_zero_bbs: Vec<(Option<SrcLoc>, BlockId)>,
    /// Locals bound by reference in patterns (they hold a pointer to the matched part).
    ref_bindings: FxHashSet<LocalId>,
    /// Locals that borrow a value other threads read at the same time: frozen params and the
    /// bindings by reference into them (frozen.rs).
    frozen: FxHashSet<LocalId>,
    /// While lowering the arguments of a call to a `Work::Frozen` instance: frozen arguments
    /// are borrowed as they are (frozen.rs).
    frozen_args: bool,
    /// The `BoxHold` the projection being lowered hands to the projection site lowered next
    /// (async_fn/sync.rs `projection_base`), which takes it.
    proj_hold: Option<async_fn::BoxHold>,
    /// The locals this function's code may change (frozen.rs `changed_locals`), once asked.
    changed: Option<FxHashSet<LocalId>>,
    /// Function params of a `Work::Frozen` instance given closure literals: their code
    /// (frozen.rs `KnownFn`).
    known_fns: FxHashMap<LocalId, frozen::KnownFn>,
    /// While binding a pattern into a frozen value: bindings take no shares (frozen.rs).
    frozen_binds: bool,
    /// While evaluating a place only to read it (`FieldPresent`): no write barrier.
    read_target: bool,
    /// The field read lowered next is about to be changed in place, reached by running code
    /// (caller_stores.rs `owner_barrier`): it puts the barrier on its owner.
    owner_write: bool,
    /// While an assignment is lowered: the box pointers of the array views its target goes
    /// through, written back after the store (array_view.rs), each with the element index
    /// written when the target is an element of it (only that element is written back).
    view_writes: Option<Vec<(Operand, TyId, Option<Operand>)>>,
    /// The next array dropped is a view's mirror, never a view itself (`Glue::MirrorDrop`).
    mirror_drop: bool,
    /// While an `AnyTo` with a text is lowered: the words its `TypeError` starts with
    /// (any_errors.rs).
    any_context: Option<String>,
    /// Converting a value a write through `any` stores in an integer type (any_number.rs).
    any_write: bool,
    /// The view bit tests and loops lowered, for versioning (view_unswitch.rs).
    view_loops: view_unswitch::ViewLoops,
    /// Where a string local's bytes are, with the number of scopes open where that was worked
    /// out: the entry goes when its block ends (str_data.rs).
    str_datas: Vec<(LocalId, Operand, usize)>,
    /// This function's read mode, once asked (rc.rs `lk_mode`).
    lk_mode: Option<Local>,
    /// This thread's `RwLock` reader state, once a `read` returned it (async_fn/sync.rs).
    rw_reader: Option<Local>,
    /// Whether this function runs in read mode (`Work::Checked`, `Work::Read`).
    mode: LkMode,
    /// A `read` callback that code outside read mode calls (`Work::ReadOwn`): the caller's
    /// variables it captured hold none of a store's objects.
    own_captures: bool,
    /// A `Work::ReadOwn` callback in a `Work::Deferred` instance (leave_levels.rs).
    own_deep: bool,
    /// The closure literal a `with`, `write` or `read` is about to call, and the read mode its
    /// code runs in.
    lock_callback: Option<(DefId, LkMode)>,
    /// The owned arguments being lowered go into the caller's containers in a `read` callback
    /// (call.rs `call_def`, caller_stores.rs `keep_for_caller`).
    args_to_caller: bool,
    /// The arguments being lowered (bit `i`: `args[i]`) that are the reader's containers a
    /// call changing the caller's containers changes (caller_stores.rs `kept_args_of`).
    kept_args: u64,
    /// The arguments being lowered (bit `i`: `args[i]`) the callee may change although they are
    /// passed `Borrow` (a modified param of a function also used as a value, call.rs
    /// `call_def`): they stay where they are, not held against later arguments.
    changed_args: u64,
    /// Those arguments as passed, handed back to the reader after the call (with whether each
    /// was the reader's own, referenced once: caller_stores.rs `hand_back`).
    kept_handed: Vec<(Operand, TyId, Option<Local>)>,
    /// Those of them the call's own values, dropped after that in the mode given.
    kept_drops: Vec<(Place, TyId, LkMode)>,
    /// What a `read` callback's body says, and how its stores go (caller_aliases.rs).
    read_info: caller_aliases::ReadInfo,
    /// With debug info: where the block declaring a local ends (debug_vars.rs).
    scope_ends: FxHashMap<LocalId, SrcLoc>,
    /// Set while lowering the body of an async function (a poll function).
    asyncx: Option<async_fn::AsyncCx>,
    /// The next call lowered is awaited or spawned right away: its promise is not started
    /// (async_fn/start.rs).
    lazy_call: bool,
    /// While lowering a stabilized borrow (stabilize.rs): every counted object a place
    /// projection goes through is retained until the end of the statement.
    retain_hops: bool,
    /// Elements of arrays borrowed in place by the arguments being lowered when the array has
    /// no other reference: checked again just before the call (stabilize.rs).
    pending_borrows: Vec<stabilize::PendingBorrow>,
    /// While binding a pattern inside a counted value: owned bindings take shares (pattern.rs).
    share_binds: bool,
    /// Set by `consume_read` for the `consume` it calls: the new owner only reads the value, so
    /// copying a tuple out of a place there is not a second name for it (boxing/tuples.rs).
    read_copy: bool,
    /// While binding a pattern to a copy of the value destructured (matching.rs `let_pat`): a
    /// binding by reference into it names a copy of the part (pattern.rs `bind_local`).
    binds_copy: bool,
    /// While binding a destructuring `const` of a place others may change (matching.rs
    /// `let_pat`): bindings by reference take owned shares instead (pattern.rs).
    stable_binds: bool,
    /// While lowering the arguments of a spawned call: owned ones are transferred (transfer.rs).
    transfer_args: bool,
    /// The next call lowered is spawned through a function value, vtable or interface: its
    /// arguments are transferred (transfer.rs). Taken by that call before anything else.
    transfer_call: bool,
    /// The direct method call being lowered has a receiver of a proxiable class: once the
    /// receiver is lowered, a proxy with a `get` trap reads the method through it
    /// (proxy_method.rs), before the other arguments.
    proxy_recv: Option<(DefId, Vec<TyId>)>,
    /// The call being lowered is `super.m(args)` (`Program::super_calls`): its receiver is not
    /// read through a proxy (`call_def` takes it).
    super_call: bool,
    /// The location of the top-level `await` lowered last (velt_sema's `top_level_await.rs`
    /// marks each with `topLevelAwaitAt(site)`): a suspension from here on tells the runtime it
    /// waits there (`velt_rt_tla_at`, async_fn/suspend.rs), only when it really suspends.
    tla_site: Option<velt_common::Span>,
    /// Building `Glue::Same`: objects inside the compared values compare by identity (same.rs).
    same_mode: bool,
    /// Comparing with `Object.is` (`Intrinsic::SameValue`, `Glue::SameValue`): floats compare
    /// with JS's SameValue, `NaN` equal to itself and `0` unequal to `-0` (same.rs).
    value_mode: bool,
    /// Comparing as keys (`Intrinsic::KeyEq`: `Map` keys, `Glue::KeySame`): floats compare with
    /// JS's SameValueZero, as `Intrinsic::Hash` hashes them (glue/eq.rs).
    key_mode: bool,
    /// Building `Glue::BitsEq` / `Glue::BitsHash`: inside a `struct` value (#641), floats compare
    /// and hash by their bits, so `==`, `Map` keys and the hash all agree with a byte compare
    /// (a `NaN` equals itself when its bits match, `-0` is not `0`).
    bits_mode: bool,
    /// Lowering an expression checked outside any body (module_state.rs): the module state's
    /// initializer (`Some(0)`), or a class field's initializer, whose locals are renumbered
    /// from this index on (`consume_detached`). Those locals (the temporaries of a `Record`
    /// literal, ...) belong to no function body: declared at their `let` (`declare_init_local`).
    init_locals: Option<u32>,
    /// While lowering a class's constructor: the (concrete) class type, whose field
    /// initializers the constructor runs (ctor_init.rs).
    ctor_self: Option<TyId>,
    /// Classes whose initializers a `new` is inlining here (ctor_init.rs): a `new` of one of
    /// them inside them calls an out-of-line initializer function instead.
    init_stack: Vec<TyId>,
    /// Closures of this body only ever called here: their envs live in the frame (frame_envs.rs).
    frame_closures: FxHashSet<DefId>,
    /// `Intrinsic::ClosureSibling` locals only ever called: calls go straight to the member
    /// (frame_envs.rs).
    direct_siblings: FxHashMap<LocalId, DefId>,
    /// The env param of the closure being lowered (`Intrinsic::ClosureSibling`).
    self_env: Option<Local>,
    /// This function loads an `Atomic` of a `shared` value, so it runs in an epoch section
    /// (epoch_sections.rs), unless it is one of `velt/sync/epoch`'s own (`epoch_exempt`).
    ref_loads: bool,
    epoch_exempt: bool,
    /// Lowering a method of the prelude's `Reflect`, where a proxy's reads reach their target:
    /// a read `any` cannot do yet names #823 (any_member.rs `any_get_str`).
    in_reflect: bool,
    /// The body of a function of a user module or of std/proxy: its operations on `any` throw
    /// what a proxy's trap throws, as velt_sema counts them (proxy_throws.rs).
    proxy_routes: bool,
    /// While a `finally` block is lowered, the number of scopes outside it: a `try` among them
    /// does not enclose its code (any_not_fn.rs).
    finally_floor: usize,
    /// The held bases of the assignment target being formed, by the address of their HIR
    /// expression (index_order.rs `target_base`).
    held_targets: Vec<(usize, Operand)>,
    /// The body of a function of a user module (not the standard library's).
    user_body: bool,
    /// How many objects are being built from a record's or object's members around this
    /// conversion (`any_record_fill`, `any_object_fill`): a member is filled the same way up to
    /// `any_record::FILL_DEPTH` deep, so a type that contains itself ends.
    record_fill: u32,
    /// The cleanup pads of this function (cleanup.rs).
    pads: cleanup::Pads,
}
