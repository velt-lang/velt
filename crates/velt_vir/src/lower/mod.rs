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
mod adt;
mod any;
mod any_checks;
mod any_delete;
mod any_eq;
mod any_glue;
mod any_has;
mod any_keys;
mod any_member;
mod any_proxy;
mod any_rest;
mod any_set;
mod any_set_site;
mod append;
mod array;
mod assign_target;
mod async_fn;
mod attempt;
mod boxes;
mod boxing;
mod call;
mod callee;
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
mod errors;
mod expr;
mod flags;
mod for_of;
mod for_of_shared;
mod foreign;
mod frame_envs;
mod frozen;
mod glue;
mod hooks;
mod intrinsics;
mod js_string;
mod json;
mod keys;
mod layout;
mod match_switch;
mod matching;
mod module_state;
mod operand;
mod ops;
mod param_any;
mod param_attrs;
mod pattern;
mod place;
mod program;
mod proxy_call;
mod proxy_method;
mod proxy_typed;
mod proxy_write;
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
mod stmt;
mod strbuf;
mod strings;
mod symbols;
mod template;
mod track_caller;
mod transfer;
mod truthy;
mod types;
mod upcast;
mod weak;
mod weak_object;
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
        cx.no_await_steps = no_await_steps;
        cx.locs = opts
            .source_map
            .map(|sm| srcloc::LocMap::new(sm, opts.std_root));
        cx.debug = (opts.debug_info && cx.locs.is_some()).then(Default::default);
        cx.seed_functions();
        while let Some((fid, work)) = cx.queue.pop_front() {
            cx.build_now(fid, &work);
        }
        // `velt_main` last: whether it selects the per-core scheduler depends on everything else.
        cx.func(Work::Main);
        while let Some((fid, work)) = cx.queue.pop_front() {
            cx.build_now(fid, &work);
        }
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
            let diags = cx.nothing_clash_diagnostics();
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
    /// The code of a method read as a value through `any` (a typed proxy's `get` trap answers
    /// a method call with it, proxy_method.rs): the method's parameters after a closure env;
    /// called on its own (without the proxy as `this`) it throws a `TypeError`.
    Unbound,
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
    /// A function instance whose params in the mask (bit `i`: `params[i]`, captures first)
    /// borrow a value other threads read at the same time (frozen.rs), caller-tracking for
    /// the call site when it is given, and given closure literals for some function params:
    /// it calls their code directly (frozen.rs `KnownFn`).
    Frozen(DefId, Vec<TyId>, u64, Option<SrcLoc>, Vec<frozen::KnownFn>),
    /// `(arr: ptr, stride: u64, align: u64)`: grow an array's buffer (amortized doubling).
    ArrayGrow,
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
    /// `(state: ptr)`: the module state's initializer (module_state.rs).
    ModuleInit,
}

/// Program-level lowering state.
struct Cx<'h> {
    hir: &'h hir::Program,
    /// Native library inits `velt_main` runs first (entry.rs).
    native_inits: Vec<crate::NativeInit>,
    /// The program pins a task (`spawn(…, { worker })`): `velt_main` selects the per-core
    /// scheduler. Known once every function
    /// but `velt_main` is lowered, which is why it is built last (`lower_program`).
    uses_placement: bool,
    /// Report every use of a captured variable's cell to the debug runtime (cells.rs,
    /// `LowerOptions::cell_checks`).
    cell_checks: bool,
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
    /// Slots are filled as the worklist is processed.
    funcs: Vec<Option<Function>>,
    work_map: FxHashMap<Work, FuncId>,
    queue: VecDeque<(FuncId, Work)>,
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
    /// Functions whose instances made an option of an option with another nothing (`T | null`
    /// at `T = U | undefined`, or the reverse; types.rs `intern`), which this phase cannot
    /// represent (issue #930): reported, not lowered silently.
    nothing_clashes: Vec<Option<DefId>>,
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
    locals: Vec<LocalDecl>,
    blocks: Vec<(Vec<vir::Stmt>, Option<Terminator>)>,
    /// Source locations parallel to `blocks`: per statement, and of the terminator.
    block_locs: Vec<(Vec<Option<SrcLoc>>, Option<SrcLoc>)>,
    /// Location recorded for statements emitted now (srcloc.rs).
    loc: Option<SrcLoc>,
    /// In a caller-tracking instance: the call site its panics report.
    caller_loc: Option<SrcLoc>,
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
    /// Lowering the module state's initializer (module_state.rs): the locals of each global's
    /// initializer (the temporaries of a `Record` literal, ...) belong to no function body and
    /// are declared at their `let` (`declare_init_local`).
    init_locals: bool,
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
    /// Lowering a method of the prelude's `Reflect`, where a proxy's reads reach their target:
    /// a read `any` cannot do yet names #823 (any_member.rs `any_get_str`).
    in_reflect: bool,
}
