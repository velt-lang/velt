//! The state lowering keeps per local and per scope: how a HIR local is represented, its
//! ownership state, and the drop obligations of each scope.

use super::*;

/// Static ownership state of a droppable local without a drop flag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LState {
    Uninit,
    Init,
    Moved,
}

/// How a HIR local is represented in VIR.
pub(super) struct LInfo {
    /// `None` for Unit-typed locals (they have no storage).
    pub(super) vir: Option<Local>,
    /// Concrete (substituted) type.
    pub(super) ty: TyId,
    /// The VIR local is a `Ptr` to the value (aggregate params, borrowed bindings, captures).
    pub(super) indirect: bool,
    /// This function owns the value and must drop it at scope exit.
    pub(super) droppable: bool,
    pub(super) flag: Option<Local>,
    pub(super) state: LState,
    /// Fields moved out of this (struct or class) local; the rest is dropped field by field.
    pub(super) moved_fields: Vec<u32>,
    /// Parts moved out of this local are replaced by their all-zero ("nothing to drop") value
    /// instead of being recorded in `moved_fields`: set when the static record would be wrong
    /// on some path (drop flags, a field move in a conditional region).
    pub(super) zero_parts: bool,
    /// The local is a shared cell (cells.rs): `vir` holds the cell pointer. When the local is
    /// `droppable` this function owns a reference, and dropping the local releases it (a
    /// captured cell's reference belongs to the closure environment).
    pub(super) cell: bool,
    /// The value lives in a shared cell (this function's or a captured one): a closure may
    /// replace it while a call borrows it (stabilize.rs).
    pub(super) in_cell: bool,
    /// The local holds a generator's state inline (async_fn/generator.rs): `vir` is the state;
    /// dropping the local closes the generator.
    pub(super) gen: Option<async_fn::GenLocal>,
    /// Nothing changes the value while the function runs: a variable of its own (not a cell,
    /// not an alias of an element) or a `Copy` param that nothing assigns (`LocalDef::mutable`
    /// false: no assignment anywhere, closures and nested functions included). A call passes
    /// its address to a `Copy` aggregate param instead of a copy (call.rs).
    pub(super) fixed: bool,
}

/// A pending drop obligation.
#[derive(Clone)]
pub(super) enum DropEntry {
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

pub(super) enum ScopeKind {
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
    /// Inside a `try` body whose `catch` takes nothing (its variable is `never`): Node's
    /// `TypeError` for a call through `any` stops here rather than skip this `catch` for an
    /// outer one (any_not_fn.rs).
    CatchesNothing,
}

pub(super) struct Scope {
    pub(super) kind: ScopeKind,
    pub(super) drops: Vec<DropEntry>,
}

impl FnLower<'_, '_> {
    /// Is `a` a local that nothing changes while this function runs (`LInfo::fixed`)?
    pub(super) fn fixed_local(&self, a: &hir::Expr) -> bool {
        match &a.kind {
            hir::ExprKind::Local(id, _) => self.info[id.0 as usize].fixed,
            _ => false,
        }
    }
}
