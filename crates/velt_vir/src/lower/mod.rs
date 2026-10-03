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
mod array;
mod async_fn;
mod attempt;
mod boxes;
mod boxing;
mod call;
mod callee;
mod cells;
mod cfg;
mod closure;
mod console;
mod ctor_init;
mod dispatch;
mod drops;
mod entry;
mod errors;
mod expr;
mod flags;
mod for_of;
mod for_of_shared;
mod foreign;
mod glue;
mod intrinsics;
mod json;
mod keys;
mod layout;
mod match_switch;
mod matching;
mod operand;
mod ops;
mod param_attrs;
mod pattern;
mod place;
mod program;
mod rc;
mod rt;
mod same;
mod share;
mod srcloc;
mod stabilize;
mod stmt;
mod strbuf;
mod strings;
mod template;
mod track_caller;
mod transfer;
mod types;
mod widen;

use std::collections::{HashMap, HashSet, VecDeque};
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
pub(crate) fn lower_program(hir: &hir::Program, opts: &LowerOptions) -> vir::Program {
    let mut types = hir.types.clone();
    let mut counted = boxing::Boxing::default();
    for _ in 0..MAX_BOXING_PASSES {
        let mut cx = Cx::new(hir, types, counted.clone());
        cx.native_inits = opts.native_inits.to_vec();
        cx.locs = opts
            .source_map
            .map(|sm| srcloc::LocMap::new(sm, opts.std_root));
        cx.seed_functions();
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
            }
            return cx.finish();
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
    EnvClone(DefId, Vec<TyId>),
    /// `(env: ptr) -> ptr`: the environment of closure `(def, targs)` made safe for another
    /// thread (glue/transfer.rs).
    EnvTransfer(DefId, Vec<TyId>),
    /// `(len: u64, index: i64|u64, at: ptr)`: index-out-of-bounds panic (true = signed index;
    /// `at` points to the ` at <location>` string suffix).
    Oob(bool),
    /// A caller-tracking function instance (track_caller.rs) for one call site.
    Tracked(DefId, Vec<TyId>, SrcLoc),
    /// `(arr: ptr, stride: u64, align: u64)`: grow an array's buffer (amortized doubling).
    ArrayGrow,
    /// The exported `velt_main() -> i32` entry (entry.rs).
    Main,
    /// `(state: ptr, cx: ptr) -> u32` poll function of an async function instance (async_fn/).
    Poll(DefId, Vec<TyId>),
    /// `(state: ptr)` drop function of an async function instance.
    AsyncDrop(DefId, Vec<TyId>),
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
}

/// Program-level lowering state.
struct Cx<'h> {
    hir: &'h hir::Program,
    /// Native library inits `velt_main` runs first (entry.rs).
    native_inits: Vec<crate::NativeInit>,
    /// Copy of the HIR type table, extended with substituted (monomorphic) types.
    types: TyTable,
    aggs: Vec<AggLayout>,
    externs: Vec<ExternFn>,
    extern_map: HashMap<String, ExternId>,
    statics: Vec<StaticData>,
    static_map: HashMap<(Vec<u8>, u32), StaticId>,
    /// Slots are filled as the worklist is processed.
    funcs: Vec<Option<Function>>,
    work_map: HashMap<Work, FuncId>,
    queue: VecDeque<(FuncId, Work)>,
    /// Functions currently being built (eager builds of poll functions detect recursion).
    building: HashSet<FuncId>,
    /// Layout facts of every async function instance whose poll function has been built.
    asyncs: HashMap<(DefId, Vec<TyId>), async_fn::AsyncInfo>,
    /// Closure instances whose declared scalar params are passed by pointer: `Mutex.with`
    /// callbacks update the locked value in place.
    by_ref_params: HashSet<(DefId, Vec<TyId>)>,
    /// Async closure instances used as http handlers: the capture modes their state machine
    /// uses, reading the shared, leaked environment (async_fn/handler.rs).
    shared_envs: HashMap<(DefId, Vec<TyId>), Vec<hir::PassMode>>,
    lay: layout::Layouts,
    /// Line tables when lowering with a source map (`lower_with`).
    locs: Option<srcloc::LocMap>,
    /// Memoized `tracks_caller` answers.
    tracked: HashMap<DefId, bool>,
    /// Interned static `VeltStr` objects (`static_str_object`).
    str_objects: HashMap<String, StaticId>,
    /// Types whose values carry a reference count in this pass (boxing/).
    boxing: boxing::Boxing,
    /// Sharing facts observed in this pass (boxing/).
    facts: boxing::Facts,
    /// `Program::impls` indexes per interface (`impls_of`), built on first use.
    iface_impls: Option<HashMap<DefId, Rc<[u32]>>>,
    /// Classes with a `clone()` of their own, and that method (`own_clone`, transfer.rs),
    /// found on first use.
    own_clones: Option<HashMap<DefId, DefId>>,
    /// Memoized `own_clone` answers per class type (only resource owners' are honoured).
    honoured_clones: HashMap<TyId, Option<DefId>>,
    /// Memoized `dyn_modes` per (interface, slot).
    dyn_modes_memo: HashMap<(DefId, u32), Option<Vec<hir::PassMode>>>,
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
    /// The local is a shared cell owned by this function (cells.rs): `vir` holds the cell
    /// pointer; dropping the local releases the cell.
    cell: bool,
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
    ref_bindings: HashSet<LocalId>,
    /// Set while lowering the body of an async function (a poll function).
    asyncx: Option<async_fn::AsyncCx>,
    /// The next call lowered is awaited or spawned right away: its promise is not started
    /// (async_fn/start.rs).
    lazy_call: bool,
    /// While lowering a stabilized borrow (stabilize.rs): every counted object a place
    /// projection goes through is retained until the end of the statement.
    retain_hops: bool,
    /// While binding a pattern inside a counted value: owned bindings take shares (pattern.rs).
    share_binds: bool,
    /// While lowering the arguments of a spawned call: owned ones are transferred (transfer.rs).
    transfer_args: bool,
    /// The next call lowered is spawned through a function value, vtable or interface: its
    /// arguments are transferred (transfer.rs). Taken by that call before anything else.
    transfer_call: bool,
    /// Building `Glue::Same`: objects inside the compared values compare by identity (same.rs).
    same_mode: bool,
    /// While lowering a class's constructor: the (concrete) class type, whose field
    /// initializers the constructor runs (ctor_init.rs).
    ctor_self: Option<TyId>,
    /// Classes whose initializers a `new` is inlining here (ctor_init.rs): a `new` of one of
    /// them inside them calls an out-of-line initializer function instead.
    init_stack: Vec<TyId>,
}
