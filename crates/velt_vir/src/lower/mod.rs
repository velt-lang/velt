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
//! - Async functions (M3) become poll/drop state machines (async_fn/); JSON glue (M4) lives in
//!   json/.
//! - Source locations (srcloc.rs): with a source map every statement records the location of
//!   the HIR expression/statement being lowered, and panics name their location
//!   (track_caller.rs for standard-library helpers such as `unwrap`).

mod abi;
mod adapters;
mod adt;
mod any;
mod any_args;
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
mod any_number;
mod any_object_fill;
mod any_proxy;
mod any_record;
mod any_rest;
mod any_set;
mod any_set_site;
mod any_tuple;
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
mod boxes;
mod boxing;
mod call;
mod callee;
mod caller_stores;
mod cells;
mod cfg;
mod class_test;
mod closure;
mod console;
mod ctor_init;
mod debug_vars;
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
mod fresh_fns;
mod frozen;
mod glue;
mod hooks;
mod impl_lookup;
mod intrinsics;
mod js_string;
mod json;
mod key_any;
mod keys;
mod layout;
mod literal_accessors;
mod match_switch;
mod matching;
mod module_state;
mod narrowed_paths;
mod operand;
mod ops;
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
mod rt;
mod same;
mod sequence;
mod share;
mod simd;
mod spread_object;
mod srcloc;
mod stabilize;
mod static_atomic;
mod stmt;
mod strbuf;
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
mod view_unswitch;
mod view_unswitch_plain;
mod weak;
mod weak_object;
mod weak_view;
mod widen;

use rustc_hash::{FxHashMap, FxHashSet};
use std::collections::VecDeque;
use std::rc::Rc;

use velt_sema::hir::{self, DefId, LocalId, TyId, TyTable};

use crate::vir::{
    self, AggLayout, BlockId, Const, ExternFn, ExternId, FuncId, Function, Local, LocalDecl,
    Operand, Place,
};
use crate::vir::{SrcLoc, StaticData, StaticId, Terminator, Ty};
use crate::LowerOptions;

pub(crate) use cfg::successors;
pub(crate) use glue::Glue;
// The VIR interpreter (tests) formats like the runtime.
use glue::VtableKey;
#[cfg(test)]
pub(crate) use glue::{inspect_key, inspect_quote};

/// Most lowering passes the counted-type fixpoint may take (boxing/): each pass adds types,
/// and the closure computed after a pass already contains everything its facts imply.
const MAX_BOXING_PASSES: usize = 8;

/// Lower a whole checked program (see [`crate::lower`]). Lowering repeats while a pass finds
/// shares of types that were not counted yet (boxing/); the type table carries over, so type
/// ids in the counted set stay valid.
pub(crate) fn lower_program(
    hir: &hir::Program,
    opts: &LowerOptions,
) -> (vir::Program, Vec<velt_common::Diagnostic>) {
    let mut types = hir.types.clone();
    let mut counted = boxing::Boxing::initial(hir);
    let mut no_await_steps = false;
    // One more pass when the `await` steps turn out to be unobservable (await_order.rs).
    for _ in 0..=MAX_BOXING_PASSES {
        let mut cx = Cx::new(hir, types, counted.clone());
        cx.native_inits = opts.native_inits.to_vec();
        cx.cell_checks = opts.cell_checks;
        cx.target = opts
            .target
            .map_or_else(crate::system::host_triple, str::to_string);
        cx.no_await_steps = no_await_steps;
        cx.locs = opts
            .source_map
            .map(|sm| srcloc::LocMap::new(sm, opts.std_root));
        cx.debug = (opts.debug_info && cx.locs.is_some()).then(Default::default);
        cx.seed_functions();
        while let Some((fid, work)) = cx.queue.pop_front() {
            cx.build_now(fid, &work);
        }
        cx.build_iface_glues();
        // `velt_main` last: whether it selects the per-core scheduler depends on everything else.
        cx.func(Work::Main);
        while let Some((fid, work)) = cx.queue.pop_front() {
            cx.build_now(fid, &work);
        }
        cx.build_iface_glues();
        let next = cx.close_boxing();
        if next == counted {
            if cx.facts.unmet {
                ice("a shared type was not counted");
            }
            if std::env::var_os("VELT_DEBUG_COUNTED").is_some() {
                eprintln!("velt: counted types: {}", cx.counted_names().join(", "));
                eprintln!("velt: weak-capable types: {}", cx.weak_names().join(", "));
            }
            if std::env::var_os("VELT_DEBUG_PROXY").is_some() {
                eprintln!("velt: proxiable types: {}", cx.proxiable_names().join(", "));
            }
            let mut diags = cx.nothing_clash_diagnostics();
            diags.extend(cx.fresh_fn_diagnostics());
            diags.extend(cx.counted_atomic_diagnostics());
            diags.extend(cx.atomic_copy_diagnostics());
            diags.extend(cx.unavailable_diagnostics());
            cx.unfreeze_module_reach();
            let types_now = cx.types.clone();
            let program = cx.finish();
            if !no_await_steps && async_fn::await_order::unobservable(&program) {
                // Lower again without the steps: the program then has no step code at all.
                no_await_steps = true;
                types = types_now;
                continue;
            }
            return (program, diags);
        }
        types = cx.types;
        counted = next;
    }
    ice("the counted types did not converge")
}

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

/// How a borrow-ABI entry point adapts its first `Ptr` argument (see glue/thunk.rs).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum ThunkKind {
    /// First param is a closure env pointer that the target (a named function) ignores. With
    /// an error type: the function value's type allows more errors than the target throws,
    /// so the thunk returns `Result<ret, E>` and converts the target's result (or errors).
    Env(Option<TyId>),
    /// First param is the receiver's data pointer (Dyn data / class object) → target's `this`.
    SelfData,
    /// First param is the data pointer of an interface value holding a closure (a generic
    /// closure as a generic function value, glue/generic_slots.rs): the closure's code runs
    /// with the closure's env.
    BoxedClosure,
    /// The code of a method read as a value through `any` (a typed proxy's `get` trap answers
    /// a method call with it, proxy_method.rs): the method's parameters after a closure env;
    /// called on its own (without the proxy as `this`) it throws a `TypeError`.
    Unbound,
}

/// When a function runs in velt_rt's read mode (boxing/locks.rs), which decides how it counts
/// and writes lock-set objects (rc.rs).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(super) enum LkMode {
    /// Never: inline counts.
    #[default]
    Plain,
    /// Always (code only a `read` callback calls directly): read mode's inline rules.
    Read,
    /// Either: asks the thread once (`velt_rt_lk_mode`).
    Checked,
    /// Plain's counts, in code that changes the caller's containers inside a `read` callback
    /// (`Work::Deferred`): an object of the lock set whose last reference goes is freed once the
    /// thread's reads end (velt_rt `velt_rt_free_after_reads`), since a reader may still point
    /// at it without counting it (read mode cannot tell it from the store's).
    Deferred,
}

/// A function the lowering must build; each distinct `Work` becomes exactly one VIR function.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Work {
    /// A user function instance (monomorphized with concrete type args).
    Fn(DefId, Vec<TyId>),
    /// Per-type compiler-generated glue.
    Glue(Glue, TyId),
    /// Borrow-ABI adapter around a function instance (function values, virtual/dyn calls).
    Thunk(ThunkKind, DefId, Vec<TyId>),
    /// Drop / clone of a heap closure environment for closure `(def, targs)`.
    EnvDrop(DefId, Vec<TyId>),
    /// `(env: ptr)`: drop the owned captures of a frame environment (frame_envs.rs), which is
    /// not freed.
    EnvDropFrame(DefId, Vec<TyId>),
    EnvClone(DefId, Vec<TyId>),
    /// `(env: ptr) -> ptr`: the environment of closure `(def, targs)` made safe for another
    /// thread (glue/transfer.rs).
    EnvTransfer(DefId, Vec<TyId>),
    /// `(len: u64, index: i64|u64, at: ptr)`: index-out-of-bounds panic (true = signed index;
    /// `at` points to the ` at <location>` string suffix).
    Oob(bool),
    /// A caller-tracking function instance (track_caller.rs) for one call site.
    Tracked(DefId, Vec<TyId>, SrcLoc),
    /// `Work` compiled to run in velt_rt's read mode as well (boxing/locks.rs): its counts and
    /// writes of lock-set objects ask the thread's mode (rc.rs). Every function whose address is
    /// taken, every callback and every poll is one; a plain `Work` is code only ever called
    /// directly from code outside read mode, which counts lock-set objects inline.
    Checked(Box<Work>),
    /// `Work` reached only by direct calls from a `read` callback literal: it always runs in read
    /// mode, so it counts lock-set objects by read mode's rules without asking.
    Read(Box<Work>),
    /// A `read` callback literal that code outside read mode calls: `Read`, and what it
    /// captured is that code's, never a store's object (`FnLower::own_captures`).
    ReadOwn(Box<Work>),
    /// `Work` changing the caller's containers inside a `read` callback (`LkMode::Deferred`).
    Deferred(Box<Work>),
    /// A function instance whose params in the mask (bit `i`: `params[i]`, captures first)
    /// borrow a value other threads read at the same time (frozen.rs), caller-tracking for
    /// the call site when it is given, and given closure literals for some function params:
    /// it calls their code directly (frozen.rs `KnownFn`).
    Frozen(DefId, Vec<TyId>, u64, Option<SrcLoc>, Vec<frozen::KnownFn>),
    /// `(arr: ptr, stride: u64, align: u64)`: grow an array's buffer (amortized doubling;
    /// `velt_rt_realloc` refuses a buffer past the array length limit).
    ArrayGrow,
    /// `()`: the panic of an array that would grow past `MAX_ARRAY_LEN` (never returns, so its
    /// calls stay cold and small).
    ArrayTooLong,
    /// The exported `velt_main() -> i32` entry (entry.rs).
    Main,
    /// `(state: ptr, cx: ptr) -> u32`: the task root that runs a synchronous `main` which reaches
    /// promises or tasks; stores its result at `state` (entry.rs).
    SyncMainRoot,
    /// `(state: ptr, cx: ptr) -> u32` poll function of an async function instance (async_fn/).
    Poll(DefId, Vec<TyId>),
    /// `(state: ptr)` drop function of an async function instance.
    AsyncDrop(DefId, Vec<TyId>),
    /// `(state: ptr)`: request the awaited close of an async generator instance (sets
    /// `CLOSE_BIT`; async_fn/generator.rs).
    AsyncCloseStart(DefId, Vec<TyId>),
    /// `(state: ptr, error: ptr) -> u32`: resume a generator instance with an error thrown in
    /// (`throw()`; async_fn/gen_send.rs).
    GenThrow(DefId, Vec<TyId>),
    /// Poll / drop of the promise-value wrapper of a throwing async function (async_fn/value.rs).
    ValuePoll(DefId, Vec<TyId>),
    ValueDrop(DefId, Vec<TyId>),
    /// Poll / drop of the heap future wrapping `velt_rt_all` for `Promise.all` with element `T`.
    AllPoll(TyId),
    AllDrop(TyId),
    /// Poll / drop of the wrapper that boxes a kept `Promise.race` over results of type `T`
    /// so it can be started (async_fn/kept.rs).
    RaceBoxPoll(TyId),
    RaceBoxDrop(TyId),
    /// Poll / drop of the wrapper widening a `Promise<T, E1>` into a `Promise<T, E2>`
    /// (async_fn/widen.rs).
    WidenPoll(TyId, TyId),
    WidenDrop(TyId, TyId),
    /// Poll / drop of the wrapper adopting a foreign result slot of an async extern's promise of
    /// type `T` (async_fn/foreign_promise.rs).
    AdoptPoll(TyId),
    AdoptDrop(TyId),
    /// `(slot: ptr)`: disposes of the unclaimed result of a started promise of a rejecting
    /// promise type (async_fn/start.rs).
    Unclaimed(TyId),
    /// `(env, req, state)` initializer of an http handler closure's per-request state.
    HandlerInit(DefId, Vec<TyId>),
    /// `(this: ptr)`: the field initializers `new` runs for class `T` (after its constructor),
    /// throwing `E`; built only for classes whose initializers construct each other in a
    /// cycle (ctor_init.rs).
    Init(TyId, Option<TyId>),
    /// Constructor of a generator instance returning its `Generator<T, E>` object, and the
    /// function freeing such an object (async_fn/gen_object.rs).
    GenNew(DefId, Vec<TyId>),
    GenFree(DefId, Vec<TyId>),
    /// `(state: ptr)`: the initializer of module state slots `from..to` (module_state.rs):
    /// the state's own (`0..deferred`), or the root file's slots its `main` initializes.
    ModuleInit(u32, u32),
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
    /// scheduler. Known once every function
    /// but `velt_main` is lowered, which is why it is built last (`lower_program`).
    uses_placement: bool,
    /// Report every use of a captured variable's cell to the debug runtime (cells.rs,
    /// `LowerOptions::cell_checks`).
    cell_checks: bool,
    /// The target triple, for system functions (`LowerOptions::target`).
    target: String,
    /// Calls of system functions the target lacks: (the declaration, the calling function, the
    /// user code being lowered then, `user_site`).
    unavailable: Vec<(DefId, Option<DefId>, velt_common::Span)>,
    /// Per user call site of a standard-library function, that function (call.rs): what an
    /// unavailable system function deep in std is reported as, at the user's call.
    std_calls: FxHashMap<velt_common::Span, DefId>,
    /// The aggregates of C structs (system.rs), by the table's struct.
    system_aggs: FxHashMap<usize, vir::AggId>,
    /// Lower no `await` steps (async_fn/await_order.rs): the program was lowered once and
    /// nothing in it can make a task's await word non-0.
    no_await_steps: bool,
    /// Copy of the HIR type table, extended with substituted (monomorphic) types.
    types: TyTable,
    aggs: Vec<AggLayout>,
    externs: Vec<ExternFn>,
    extern_map: FxHashMap<String, ExternId>,
    statics: Vec<StaticData>,
    /// The module state's descriptor, once a slot is used (module_state.rs).
    module_desc: Option<StaticId>,
    /// The module state slots some code assigns as a whole, once asked (module_state.rs).
    reassigned_slots: Option<FxHashSet<hir::DefId>>,
    /// The prelude's `Error` class, once looked up (js_string.rs).
    prelude_error: Option<Option<hir::DefId>>,
    /// The prelude's top-level `await` mark and what it forwards to (async_fn/suspend.rs).
    tla_marks: Option<[Option<hir::DefId>; 2]>,
    static_map: FxHashMap<(Vec<u8>, u32), StaticId>,
    /// The stored elements of each module constant table read so far (`table_read`): the data
    /// and the length, or `None` for a constant that is not one.
    tables: FxHashMap<hir::DefId, Option<(StaticId, usize)>>,
    /// The data of each static atomic used so far, `None` for a global that is not one
    /// (static_atomic.rs).
    static_atomics: FxHashMap<hir::DefId, Option<StaticId>>,
    /// Slots are filled as the worklist is processed.
    funcs: Vec<Option<Function>>,
    work_map: FxHashMap<Work, FuncId>,
    queue: VecDeque<(FuncId, Work)>,
    /// The glues that depend on the types `any` values hold (`AnyIface`, any_iface.rs, and
    /// `AnyNumeric`, any_number.rs), built last: each with the number of such types when it
    /// was built.
    iface_glues: Vec<(FuncId, Work, Option<usize>)>,
    /// Params given `noalias` or `frozen`, with their types, for `unfreeze_module_reach`
    /// (boxing/module_reach.rs); those of the function being declared until it is built.
    unique_params: Vec<(FuncId, u32, TyId)>,
    declared_unique: Vec<(u32, TyId)>,
    /// Functions currently being built (eager builds of poll functions detect recursion).
    building: FxHashSet<FuncId>,
    /// Layout facts of every async function instance whose poll function has been built.
    asyncs: FxHashMap<(DefId, Vec<TyId>), async_fn::AsyncInfo>,
    /// Closure instances whose declared scalar params are passed by pointer: `Mutex.with`
    /// callbacks update the locked value in place.
    by_ref_params: FxHashSet<(DefId, Vec<TyId>)>,
    /// Async closure instances used as http handlers: the capture modes their state machine
    /// uses, reading the shared, leaked environment (async_fn/handler.rs).
    shared_envs: FxHashMap<(DefId, Vec<TyId>), Vec<hir::PassMode>>,
    lay: layout::Layouts,
    /// Line tables when lowering with a source map (`lower_with`).
    locs: Option<srcloc::LocMap>,
    /// Debug types of source variables (`LowerOptions::debug_info`, debug_vars.rs).
    debug: Option<debug_vars::DebugTypes>,
    /// Memoized `tracks_caller` answers.
    tracked: FxHashMap<DefId, bool>,
    /// HIR nodes of the standard-library functions `tracks_caller` scanned (track_caller.rs).
    tracked_size: FxHashMap<DefId, usize>,
    /// Memoized `quiet` answers (quiet.rs).
    quiet: FxHashMap<DefId, bool>,
    /// Interned static `VeltStr` objects (`static_str_object`).
    str_objects: FxHashMap<String, StaticId>,
    /// The records of the compiler's symbols (`Intrinsic::SymbolStatic`, symbols.rs).
    symbol_records: symbols::SymbolRecords,
    /// Types whose values carry a reference count in this pass (boxing/).
    boxing: boxing::Boxing,
    /// Sharing facts observed in this pass (boxing/).
    facts: boxing::Facts,
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
    /// Whether some class has a `toJSON(key)`, and memoized `json_needs_key` per type
    /// (json/key.rs); `None` until first asked.
    json_key_memo: Option<(bool, FxHashMap<TyId, bool>)>,
    /// One type per anonymous object shape (types.rs `canon`).
    anon: types::AnonShapes,
    /// The function whose instance is being built (for `nothing_clashes`).
    cur_def: Option<DefId>,
    /// The read mode of the function being built (`Work::Checked`, `Work::Read`).
    cur_mode: LkMode,
    /// Whether the function being built is a `Work::ReadOwn`.
    cur_own: bool,
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
    /// The shared type each type `close_boxing` reached through shared values came from.
    share_from: rustc_hash::FxHashMap<TyId, TyId>,
    /// Non-zero while the field types of a prelude `Map` are substituted (types.rs
    /// `keeps_nothings_apart`).
    map_fields: u32,
}

/// Static ownership state of a droppable local without a drop flag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LState {
    Uninit,
    Init,
    Moved,
}

/// How a HIR local is represented in VIR.
struct LInfo {
    /// `None` for Unit-typed locals (they have no storage).
    vir: Option<Local>,
    /// Concrete (substituted) type.
    ty: TyId,
    /// The VIR local is a `Ptr` to the value (aggregate params, borrowed bindings, captures).
    indirect: bool,
    /// This function owns the value and must drop it at scope exit.
    droppable: bool,
    flag: Option<Local>,
    state: LState,
    /// Fields moved out of this (struct or class) local; the rest is dropped field by field.
    moved_fields: Vec<u32>,
    /// Parts moved out of this local are replaced by their all-zero ("nothing to drop") value
    /// instead of being recorded in `moved_fields`: set when the static record would be wrong
    /// on some path (drop flags, a field move in a conditional region).
    zero_parts: bool,
    /// The local is a shared cell (cells.rs): `vir` holds the cell pointer. When the local is
    /// `droppable` this function owns a reference, and dropping the local releases it (a
    /// captured cell's reference belongs to the closure environment).
    cell: bool,
    /// The value lives in a shared cell (this function's or a captured one): a closure may
    /// replace it while a call borrows it (stabilize.rs).
    in_cell: bool,
    /// The local holds a generator's state inline (async_fn/generator.rs): `vir` is the state;
    /// dropping the local closes the generator.
    gen: Option<async_fn::GenLocal>,
}

/// A pending drop obligation.
#[derive(Clone)]
enum DropEntry {
    Local(LocalId),
    /// Owned temporary value at a place, with its concrete type for drop glue.
    Temp(Place, TyId),
    /// A class object `new` is constructing (adt.rs): on a throw from its constructor or field
    /// initializers its fields drop and it is freed, but its `[Symbol.dispose]()` does not run.
    /// Becomes a `Temp` once constructed.
    HalfBuilt(Place, TyId),
    /// Owned temporary value that holds something only when the `Bool` local is true.
    Flagged(Place, TyId, vir::Local),
    /// An owned value from which a pattern moved some parts: drop everything else.
    Rest(Place, TyId, Rc<hir::Pat>),
    /// An array consumed by `for…of`: elements `next..len` (of type `elem`) are still owned,
    /// the ones before were moved out (for_of.rs).
    ConsumedArray {
        arr: Place,
        next: Local,
        elem: TyId,
    },
}

enum ScopeKind {
    Block,
    /// Statement-level temporaries, dropped when the statement finishes.
    Temps,
    Loop {
        label: Option<String>,
        brk: BlockId,
        cont: BlockId,
    },
    /// Inside a `try` body with a `catch`: errors are stored to `slot` and jump to `handler`.
    Try {
        handler: BlockId,
        slot: Local,
        /// The `catch` variable's (concrete) type: errors are converted to it.
        ty: TyId,
    },
    /// Inside a `try` with `finally`: the block runs on every exit (taken while being lowered).
    Finally(Option<Rc<hir::Block>>),
    /// A module state initializer: an error thrown in it and not caught there prints
    /// `Uncaught …` and ends the process with 1, as Node stops at an error thrown while it
    /// evaluates a module (module_state.rs).
    Uncaught,
}

struct Scope {
    kind: ScopeKind,
    drops: Vec<DropEntry>,
}

/// Per-function lowering state.
struct FnLower<'c, 'h> {
    cx: &'c mut Cx<'h>,
    /// Type arguments substituted for `TyKind::Param` in the HIR being lowered.
    targs: Vec<TyId>,
    /// Building a glue function: its comparisons are counted where it is called (fresh_fns.rs).
    glue: bool,
    locals: Vec<LocalDecl>,
    blocks: Vec<(Vec<vir::Stmt>, Option<Terminator>)>,
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
    /// The view bit tests and loops lowered, for versioning (view_unswitch.rs).
    view_loops: view_unswitch::ViewLoops,
    /// This function's read mode, once asked (rc.rs `lk_mode`).
    lk_mode: Option<Local>,
    /// This thread's `RwLock` reader state, once a `read` returned it (async_fn/sync.rs).
    rw_reader: Option<Local>,
    /// Whether this function runs in read mode (`Work::Checked`, `Work::Read`).
    mode: LkMode,
    /// A `read` callback that code outside read mode calls (`Work::ReadOwn`): the caller's
    /// variables it captured hold none of a store's objects.
    own_captures: bool,
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
    /// A `read` callback's variables naming the caller's objects (caller_stores.rs
    /// `caller_alias`), once asked.
    caller_aliases: Option<FxHashSet<LocalId>>,
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
    /// marks each with `__topLevelAwaitAt(site)`): a suspension from here on tells the runtime it
    /// waits there (`velt_rt_tla_at`, async_fn/suspend.rs), only when it really suspends.
    tla_site: Option<velt_common::Span>,
    /// Building `Glue::Same`: objects inside the compared values compare by identity (same.rs).
    same_mode: bool,
    /// Comparing with `Object.is` (`Intrinsic::SameValue`, `Glue::SameValue`): floats compare
    /// with JS's SameValue, `NaN` equal to itself and `0` unequal to `-0` (same.rs).
    value_mode: bool,
    /// Comparing as keys (`Intrinsic::KeyEq`: `Map` keys, `Glue::KeySame`; `Intrinsic::Eq`:
    /// `deepEqual`, `Glue::KeyEq`): floats compare with JS's SameValueZero, as
    /// `Intrinsic::Hash` hashes them (glue/eq.rs).
    key_mode: bool,
    /// Comparing as Vitest's `toEqual` (`Intrinsic::DeepEq`, `Glue::DeepEq`): as `Intrinsic::Eq`,
    /// but class instances by their fields and floats with SameValue (glue/eq.rs).
    deep_mode: bool,
    /// Lowering an expression checked outside any body (module_state.rs): the module state's
    /// initializer (`Some(0)`), or a class field's initializer, whose locals are renumbered
    /// from this index on (`consume_detached`). Those locals (the temporaries of a `Record`
    /// literal, ...) belong to no function body and are declared at their `let`
    /// (`declare_init_local`).
    init_locals: Option<u32>,
    /// While lowering a class's constructor: the (concrete) class type, whose field
    /// initializers the constructor runs (ctor_init.rs).
    ctor_self: Option<TyId>,
    /// Classes whose initializers a `new` is inlining here (ctor_init.rs): a `new` of one of
    /// them inside them calls an out-of-line initializer function instead.
    init_stack: Vec<TyId>,
    /// Closures of this body only ever called here: their envs live in the frame
    /// (frame_envs.rs).
    frame_closures: FxHashSet<DefId>,
    /// `Intrinsic::ClosureSibling` locals only ever called: calls go straight to the member
    /// (frame_envs.rs).
    direct_siblings: FxHashMap<LocalId, DefId>,
    /// The env param of the closure being lowered (`Intrinsic::ClosureSibling`).
    self_env: Option<Local>,
    /// This function loads an `Atomic` of a `shared` value, so it runs in an epoch section
    /// (epoch_sections.rs), unless it is one of `velt:sync/epoch`'s own (`epoch_exempt`).
    ref_loads: bool,
    epoch_exempt: bool,
    /// Lowering a method of the prelude's `Reflect`, where a proxy's reads reach their target:
    /// a read `any` cannot do yet names #823 (any_member.rs `any_get_str`).
    in_reflect: bool,
    /// The body of a function of a user module or of std/proxy: its operations on `any` throw
    /// what a proxy's trap throws, as velt_sema counts them (proxy_throws.rs).
    proxy_routes: bool,
    /// An object is being built from a record's entries (`any_record_fill`): its fields are
    /// converted without that path, so a type that contains itself ends.
    record_fill: bool,
}
