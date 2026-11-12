//! What the lowering builds: one [`Work`] for each VIR function.

use velt_sema::hir::{DefId, TyId};

use super::frozen::KnownFn;
use super::{Glue, ThunkKind};
use crate::vir::SrcLoc;

/// A function the lowering must build; each distinct `Work` becomes exactly one VIR function.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum Work {
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
    /// `(code, env, out: ptr)`: the function the function value `{ code, env }` leads to through
    /// wrappers, written to `out` (closure/fn_wrappers.rs).
    FnRoot,
    /// `(a: ptr, b_code, b_env) -> bool`: `===` of two function values through wrappers
    /// (closure/fn_wrappers.rs).
    FnSame,
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
    Frozen(DefId, Vec<TyId>, u64, Option<SrcLoc>, Vec<KnownFn>),
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
    /// `(state: ptr) -> i32`: the code after the root of `main` is done, on a JS host (entry.rs).
    MainFinish,
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
    /// `()`: releases the references the static atomics hold, at exit (static_atomic.rs).
    StaticsDrop,
    /// An access of module state (module_state.rs `Access`), through the executor in Velt's
    /// hooks or the runtime's entries: built once the rest of the program is.
    ModuleAccess(super::module_state::Access),
    /// The C function of `Program::c_exports[i]` (`@export("c_name")`, c_export.rs): its
    /// unmangled symbol, its C signature, a call of the Velt function.
    CExport(u32),
}
