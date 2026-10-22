//! Pass 2: per-function name resolution + bidirectional type checking + desugaring into HIR.
//!
//! # Encoding decisions (other stages rely on these)
//! - **void `main`** (and every void fn) has `FnDef::ret == Unit`; no trailing `return` is inserted.
//! - **Use modes**: every read of a place has a `UseMode`. Copy types get `UseMode::Copy`.
//!   Non-Copy reads get `Move` for `let` initializers, `return` values, assignment RHS, literal
//!   elements/fields, `throw`, args to `Owned` params and ternary branches; `Borrow` for args to
//!   `Borrow` params, `console.log/error` args, operator operands, conditions and expression
//!   statements; `BorrowMut` for args to params and receivers known to be modified (intrinsics,
//!   setters, constructors); calls to user functions are patched once mutation is inferred.
//! - **Places** (`Local`/`Field`/`Index`/`UnwrapSome`/`Global`): the outermost node's mode says
//!   how the value is used; the base of a projection is `Borrow` (or `BorrowMut` when the
//!   projection is written / mutably borrowed) even for Copy base types. Moving a field out of a
//!   struct local (`Field { mode: Move }`) is a partial move: the other fields must still be
//!   dropped at scope end. Assignment places use `BorrowMut`.
//! - **Method calls** are `Call { Callee::Def(method, type_args), [receiver, args..] }`; the
//!   receiver uses the `this` pass mode (`Borrow`/`BorrowMut`, also for Copy types; `Owned` →
//!   `Move`/`Copy`). `type_args` = the owner's generics (class / extend block / interface +
//!   implementor) followed by the method's own.
//! - **Closures** capture into leading params (see `expr/closure.rs`); function values are
//!   `TyKind::FnPtr` and not Copy (they may own their captured state).
//! - **Ownership / mutation inference** runs after all bodies (`crate::ownership`): params start
//!   `Copy` / `Borrow` and may become `BorrowMut` or `Owned`; call sites are patched.
//! - **Strings**: `a + b` → `Call(Intrinsic::StrConcat, [a, b])`; `s += e` →
//!   `Assign { s, StrConcat(s (Borrow), e) }`. `StrConcat` operands may be owned temporaries.
//! - **Templates**: left fold of `StrConcat` over the non-empty parts; string-typed `${e}` parts are
//!   used directly (Borrow), others wrapped in `ToString` (any printable type). A template with no
//!   parts is `Lit("")`; exactly one string `${e}` is `StrConcat(Lit(""), e)` (a fresh value).
//! - **`++`/`--`**: as a statement → `CompoundAssign { Add|Sub, place, 1 }`. Prefix as a value →
//!   `Block { [CompoundAssign], value: place read }`. Postfix as a value →
//!   `Block { [Let tmp = place; CompoundAssign], value: Local(tmp) }` (tmp named `<postfix>`).
//! - **`for(init; cond; step)`** → `Block { init; While { cond, body, step } }`; missing cond →
//!   `Lit(true)`. **`do body while (c)`** → see the hir.rs header (two forms).
//! - **Ternary** → `ExprKind::If`. Statement `if` without braces is a one-statement block.

mod arrow_scans;
mod assign_ends;
mod assign_narrow;
pub(crate) mod assigned;
mod closure_assigned;
pub(crate) mod closure_group;
mod const_borrow;
mod consume;
mod ctor;
pub(crate) mod defaults;
mod driver;
pub(crate) mod expr;
mod field_narrow;
mod for_any;
mod for_await;
pub(crate) mod for_in_proxy;
mod for_iter;
mod for_map;
mod gated;
mod generators;
mod index_keys;
mod late_consts;
pub(crate) mod literal_hosts;
pub(crate) mod literal_locals;
pub(crate) use generators::{weak_copy_error, GenCopy, LOCK_NOTE};
mod chain_facts;
mod literal_patterns;
mod locals;
mod loops;
pub(crate) mod moved_const;
pub(crate) mod narrow;
mod nested_pattern;
mod object_rest;
mod param_patterns;
mod pattern;
mod pattern_defaults;
pub(crate) mod places;
mod predicates;
mod property_pattern;
pub(crate) mod pure_init;
mod readonly_arrays;
pub(crate) mod recheck;
pub(crate) mod recursion;
pub(crate) mod returns;
mod stmt;
pub(crate) use readonly_arrays::is_readonly_array;
pub(crate) use stmt::closure_def;
pub(crate) mod switch;
mod untyped_let;
mod using;

use rustc_hash::FxHashMap;

use velt_common::{Diagnostic, Span};

use crate::ctx::Ctx;
use crate::defs::{Bound, FnKind, ThrowSrc};
use crate::hir::{self, DefId, LocalDef, LocalId, TyId, UseMode};
use crate::resolve::TyEnv;

pub(crate) use defaults::param_defaults;
pub(crate) use driver::{check_bodies, ensure_body, ensure_global, field_defaults, printable};
pub(crate) use expr::REPORTED;
pub(crate) use pattern_defaults::UNDEFINED_OR;

/// What the consumer of an expression's value does with it (only matters for non-Copy types).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Want {
    Move,
    Borrow,
    BorrowMut,
}

/// Role of a local in its function (drives mutability and move-out rules).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LocalKind {
    Let,
    Const,
    /// `using` / `await using`: a `const` that stays in place until the end of its block (it
    /// cannot be moved out; `crate::moves`).
    Using,
    Param,
    This,
    Temp,
    /// Pattern binding of a `match` arm or destructuring `let` (binding mode is inferred).
    Bind,
    /// `for...of` element binding (borrows the element).
    Elem,
    /// A captured variable inside a closure body.
    Capture,
}

/// An enclosing loop or `switch` (a `break` target).
#[derive(Clone)]
pub(crate) struct LoopCx {
    pub label: Option<String>,
    pub has_continue: bool,
    /// A `switch` (`break` exits it; `continue` skips it and targets the loop around it).
    pub is_switch: bool,
    /// Some `break` targets this entry.
    pub has_break: bool,
    /// A `continue` from inside a `switch` targets this loop: its HIR loop needs a label.
    pub needs_label: bool,
    /// Label given to the HIR loop when the source has none but one is needed.
    pub synth_label: String,
    /// The number of scopes open where the entry starts.
    pub depth: usize,
    /// For a `switch`: the narrowing state at each `break` that leaves it, folded into the
    /// scopes open at the `switch` (`switch::case_bodies` joins them).
    pub break_states: Vec<locals::NarrowState>,
}

impl LoopCx {
    /// The label of the HIR loop built for this entry.
    pub fn hir_label(&self) -> Option<String> {
        match &self.label {
            Some(l) => Some(l.clone()),
            None if self.needs_label || self.is_switch => Some(self.synth_label.clone()),
            None => None,
        }
    }
}

#[derive(Default, Clone)]
pub(crate) struct Scope {
    pub names: FxHashMap<String, LocalId>,
    /// Option locals narrowed to their payload inside this scope.
    pub narrowed: Vec<LocalId>,
    /// `T | null | undefined` locals known not to hold this nothing inside this scope.
    pub excluded: Vec<(LocalId, crate::hir::Nothing)>,
    /// Union locals narrowed to some of their variants inside this scope (see `narrow::Fact`).
    pub members: Vec<(LocalId, Vec<u32>)>,
    /// Class (or interface) locals known by `instanceof` to hold this subclass inside this scope.
    pub classes: Vec<(LocalId, TyId)>,
    /// Facts not assumed inside this scope: their locals are assigned by closures
    /// (`closure_assigned`).
    pub refused: Vec<(LocalId, closure_assigned::Refused)>,
    /// Source offset where the scope ends (its locals' visibility, for `crate::ide`).
    pub hi: u32,
}

/// A captured variable of a closure frame.
#[derive(Clone)]
pub(crate) struct CaptureCx {
    pub outer: LocalId,
    pub inner: LocalId,
    pub mutated: bool,
    /// Where the closure first mutates it (for async closures, where that is an error).
    pub mutated_at: Option<Span>,
    /// The variable was narrowed where the closure was created, and the closure body starts
    /// from that narrowing (so the body must not assign it).
    pub narrowed: bool,
}

/// A promise an async function returns (`expr::tasks`, `async_returned`): the `return` stores
/// it in `slot` (`T | null`) and leaves the body's scopes with `break` [`ADOPT_LABEL`]; then
/// `value` (reading `bind`, the promise moved out of `slot`) is returned.
#[derive(Clone)]
pub(crate) struct Adopted {
    pub slot: LocalId,
    pub bind: LocalId,
    pub value: hir::Expr,
}

/// The label of the loop around the body of an async function that returns a promise.
pub(crate) const ADOPT_LABEL: &str = "<adopt>";

/// Per-function checking state; closures push a new frame (the enclosing one is saved).
#[derive(Clone)]
pub(crate) struct Frame {
    pub kind: FnKind,
    /// The CPU extension this function is gated on (`__intrinsic_target`, `body::gated`): it
    /// may call the gated functions of that target. A closure's frame starts without it.
    pub target: Option<crate::hir::CpuTarget>,
    /// The checks of CPU extensions that hold where the code being checked runs: the `then`
    /// branches of the `if (u32x4.hasShaArm())` being checked, innermost last.
    pub cpu_guards: Vec<crate::hir::CpuTarget>,
    pub locals: Vec<LocalDef>,
    pub kinds: Vec<LocalKind>,
    pub scopes: Vec<Scope>,
    pub loops: Vec<LoopCx>,
    /// Loops and `switch`es entered so far (numbers synthesized labels).
    pub loop_count: u32,
    /// Declared/expected return type; `None` while it is inferred from the body's `return`s
    /// (recorded in `returns`).
    pub ret: Option<TyId>,
    pub returns: returns::Returns,
    pub captures: Vec<CaptureCx>,
    pub escaping: bool,
    /// An arrow function whose `void` result comes from the expected function type, not from an
    /// annotation: as in TypeScript, the value its body or a `return` gives is evaluated and
    /// dropped (`(s: string) => void` takes `(s) => out.push(s)` whatever `push` returns).
    pub discards_value: bool,
    /// Body of an `async` function / arrow: `await` is allowed.
    pub is_async: bool,
    /// Body of a generator: the type of the values it yields (`yield` is allowed).
    pub yield_ty: Option<TyId>,
    /// Body of a `Generator<T, E, N>`: `N`, the type of a `yield`'s value (`None`: it has
    /// none).
    pub next_ty: Option<TyId>,
    /// Nesting depth of the `try` blocks with a `catch` being checked: in a program that calls
    /// a generator's `throw()`, a `yield` in one may throw (`generators.rs`).
    pub catch_depth: u32,
    /// Body of a named generator function expression: its name (not in scope there).
    pub fn_expr_name: Option<String>,
    /// Nesting depth of the `finally` blocks being checked (`yield` is not allowed in them).
    pub finally_depth: u32,
    /// The promises this async function returns (`expr::tasks`, `async_returned`), awaited
    /// once the body's scopes are left (`wrap_adopted`; issue #1148).
    pub adopted: Vec<Adopted>,
    /// Which of `adopted` was stored last (`i + 1`; `0` for none): every store writes it, so a
    /// `return p` cancelled by a `finally` (`continue`, `break`, another `return`) is not
    /// adopted, as in JS.
    pub adopt_which: Option<LocalId>,
    /// The `return` being checked stores a promise to adopt: it becomes that statement.
    pub adopt_stmt: bool,
    /// In a generator's `finally` block: the loop stack's length when it was entered (a
    /// `break`/`continue` there cannot target a loop outside it).
    pub finally_loops: Option<usize>,
    /// Spans (`lo`, `hi`) of the `yield`s about to be checked whose value is unused: those
    /// that are, or end, an expression statement (`generators.rs`, `stmt_yields`).
    pub stmt_yields: rustc_hash::FxHashSet<(u32, u32)>,
    /// See `FnInfo::soft_moves`.
    pub soft_moves: Vec<Span>,
    /// `using` variables passed to an async call (receiver or argument), not yet soft moves:
    /// only a directly awaited call may share one (`expr::tasks`, `using_share`).
    pub using_shares: Vec<(Span, LocalId)>,
    /// The `using` locals declared `await using`.
    pub await_using: rustc_hash::FxHashSet<LocalId>,
    /// Locals declared with a read-only array type (`readonly T[]`, `ReadonlyArray<T>`), which
    /// the body may not write to (`readonly_arrays.rs`).
    pub readonly_arrays: rustc_hash::FxHashSet<LocalId>,
    /// `const c = [2, "y"] as const` (literals of different types): its elements, which an
    /// array pattern taking `c` apart reads as the literal (`literal_patterns`).
    pub as_const_tuples: FxHashMap<LocalId, Vec<velt_syntax::ast::Expr>>,
    /// `const f = (…) => …`: the closure each such local holds, whose parameter defaults a
    /// call `f(…)` fills in.
    pub closure_consts: rustc_hash::FxHashMap<LocalId, DefId>,
    /// The property writes after the declaration being checked, which a cast of a function to
    /// a callable object type there takes as members (`expr::callables`, #921).
    pub callable_writes: Option<expr::PendingWrites>,
    /// Variables made functions with properties by `Object.assign(f, { … })`, with the call's
    /// span (`expr::callables`).
    pub assign_targets: Vec<(LocalId, Span)>,
    /// Locals declared with a method read as a value (`const f = o.m`): a call that passes
    /// them another `this` is not supported yet (`call_apply`).
    pub method_values: rustc_hash::FxHashSet<LocalId>,
    /// Throw sources of the enclosing `try` bodies (innermost last).
    pub tries: Vec<Vec<ThrowSrc>>,
    pub uncaught: Vec<ThrowSrc>,
    /// `super(...)` is allowed here: a root-level statement of a constructor that is the call
    /// itself, before any other `super(...)` (`stmt` sets it; the call takes it).
    pub super_ok: bool,
    pub super_called: bool,
    /// The `super(...);` statements of a derived constructor that may call the base
    /// constructor: root-level ones, and those in `if` / `else` branches that each call it
    /// once (`ctor::super_sites`).
    pub super_sites: Vec<Span>,
    /// `super(...);` statements in branches already reported as one error (`ctor`).
    pub super_silent: Vec<Span>,
    /// A derived class's constructor before its `super(...)` call: `this` and `super.x` are
    /// errors, as is `return` (`driver` sets it; the call clears it).
    pub before_super: bool,
    /// How many statements enclose the one being checked (1 at the body's root).
    pub stmt_depth: u32,
    /// Field paths that conditions narrow (`field_narrow`).
    pub field_tokens: Vec<field_narrow::FieldToken>,
    /// `const`s bound by reference (`const_borrow`).
    pub const_refs: rustc_hash::FxHashSet<LocalId>,
    /// Tokens of field paths tested by `instanceof` that are not narrowed (a field on the path
    /// is not `readonly`), and the reads of them since (`field_narrow`, for error notes).
    pub mutable_tests: Vec<LocalId>,
    pub unnarrowed_reads: Vec<Span>,
    /// Names of the variables that closures created in this function's body assign, with
    /// where (`closure_assigned`): they are not narrowed.
    pub closure_assigned: FxHashMap<String, Span>,
    /// Where this function's own code last assigns each variable (`assign_ends`): closures
    /// created before that do not see the variable's narrowing.
    pub assign_ends: FxHashMap<String, u32>,
    /// A callback of the JS API returning a number: an integer it returns converts
    /// (`returns::returned`).
    pub int_returns_number: bool,
    /// A predicate of the JS API (`filter`, `find`, `some`, ...) without a declared result
    /// type: what it returns is tested for truthiness, as JS does (`returns::returned`).
    pub truthy_returns: bool,
    /// `const k = "a"` without a type: the literal each such local holds, which a `case k:`
    /// selects like the literal itself (TypeScript gives the constant the literal type).
    pub const_lits: FxHashMap<LocalId, velt_syntax::ast::SignedLit>,
    /// `const re = /…/` (or `new RegExp` of literals, or another such local): the number of
    /// capturing groups and the flags of the regex each such local holds (`regex_args`).
    pub regex_consts: FxHashMap<LocalId, (usize, String)>,
    /// `let x;` without a type or initializer, not assigned yet: where each is declared. The
    /// first assignment gives it its type (`untyped_let`).
    pub untyped_lets: FxHashMap<LocalId, Span>,
    /// `let x: T;` without an initializer that a closure assigns (`closure_assigned`): held as
    /// a `T | undefined` that starts `undefined`, and read through a check
    /// ([`FnCx::late_let_read`]).
    pub late_lets: rustc_hash::FxHashSet<LocalId>,
    /// `const { value, done } = r` of a union discriminated by the `bool` field `done`
    /// (`IteratorResult<T>`): per `done` binding, the other bindings of the pattern that are not
    /// null when `done` holds the given value (`property_pattern.rs`, issue #1114).
    pub destructured_discriminants: FxHashMap<LocalId, Vec<(bool, LocalId)>>,
    /// Calls of type predicates (`isFoo(x)`) whose conditions narrow a local: by the call's
    /// span (`lo`, `hi`), the local and the facts it establishes when the call returns `true`
    /// and `false`. The call checks them at run time (`predicates::predicate_checked`).
    pub checked_predicates: FxHashMap<(u32, u32), predicates::CheckedPredicate>,
    /// Recursive closure groups declared in this frame, or the one whose member it checks
    /// (`closure_group`).
    pub groups: closure_group::GroupFrame,
    /// `const`s used by closures in their own initializer (`late_consts`).
    pub late: late_consts::LateFrame,
}

impl Frame {
    pub fn new(kind: FnKind, ret: Option<TyId>) -> Self {
        Frame {
            kind,
            target: None,
            cpu_guards: vec![],
            locals: vec![],
            kinds: vec![],
            scopes: vec![Scope::default()],
            loops: vec![],
            loop_count: 0,
            ret,
            returns: Default::default(),
            captures: vec![],
            escaping: false,
            discards_value: false,
            is_async: false,
            yield_ty: None,
            next_ty: None,
            catch_depth: 0,
            fn_expr_name: None,
            finally_depth: 0,
            adopted: vec![],
            adopt_which: None,
            adopt_stmt: false,
            finally_loops: None,
            stmt_yields: Default::default(),
            soft_moves: vec![],
            using_shares: vec![],
            await_using: Default::default(),
            readonly_arrays: Default::default(),
            as_const_tuples: Default::default(),
            closure_consts: Default::default(),
            callable_writes: None,
            assign_targets: vec![],
            method_values: Default::default(),
            tries: vec![],
            uncaught: vec![],
            super_ok: false,
            super_called: false,
            super_sites: vec![],
            super_silent: vec![],
            before_super: false,
            stmt_depth: 0,
            field_tokens: vec![],
            const_refs: Default::default(),
            mutable_tests: vec![],
            unnarrowed_reads: vec![],
            closure_assigned: FxHashMap::default(),
            assign_ends: FxHashMap::default(),
            int_returns_number: false,
            checked_predicates: FxHashMap::default(),
            truthy_returns: false,
            const_lits: FxHashMap::default(),
            regex_consts: FxHashMap::default(),
            untyped_lets: FxHashMap::default(),
            late_lets: Default::default(),
            destructured_discriminants: FxHashMap::default(),
            groups: Default::default(),
            late: Default::default(),
        }
    }
}

/// What an `undefined` would be given for (`FnCx::optional_slots`).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum OptionalSlot {
    Param,
    Field,
}

pub(crate) struct FnCx<'a, 'm> {
    pub cx: &'a mut Ctx<'m>,
    pub module: usize,
    /// Generic params in scope (for annotations inside the body).
    pub env: TyEnv,
    pub bounds: Vec<Vec<Bound>>,
    /// Type bounds of the generic params in scope (`Generics::constraints`).
    pub constraints: Vec<Option<TyId>>,
    /// Name of the enclosing top-level function (for closure names).
    pub fn_name: String,
    /// Type whose body is being checked (methods, constructors, defaults, field initializers):
    /// its `private` members are accessible.
    pub owner: Option<DefId>,
    /// The function whose body is being checked, if it is a declared function (for a nested
    /// one, `Ctx::enclosing_class` gives the class whose body it is in).
    pub body_def: Option<DefId>,
    /// In a static method using `this` or `super` (`collect::static_this`): the class `this`
    /// is, and the class declaring the method (whose base `super` is).
    pub static_this: Option<(DefId, DefId)>,
    /// Locals of the functions enclosing a nested declaration (see `collect::nested`).
    pub enclosing_locals: Vec<String>,
    /// The body is a local generic arrow function (checked as a nested function).
    pub generic_arrow: bool,
    pub f: Frame,
    /// Enclosing frames of the closure being checked (innermost last).
    pub outer: Vec<Frame>,
    /// The span of a `new Promise` that is the operand of the `await` being checked.
    pub direct_await: Option<Span>,
    /// The arrow being checked is an argument of a JS API function called from user code
    /// (`numbers::is_js_api`): per parameter, whether the signature declares it an integer (an
    /// index), which makes it a number in the arrow's body when unannotated.
    pub std_callback: Option<Vec<bool>>,
    /// The value being checked fills slot `.1` of interface `.0` in an object literal implementing
    /// it, and is written at `.2` (`expr/iface_object.rs`): a closure written there records
    /// what it throws for the slot before its error type is computed.
    pub literal_slot: Option<(DefId, u32, Span)>,
    /// Hidden temporaries of the call being checked, evaluated before it (`spread_args`): the
    /// call's expression is a block with these first (`FnCx::expr`).
    pub call_temps: Vec<hir::Stmt>,
    /// The tuple places of an assignment target `p![i]` / `(p as T)[i]` that writes in place
    /// (`expr/assign_paths.rs`), outermost last: checked again, in order, after a right-hand
    /// side with effects.
    pub held_recheck: Vec<expr::assign_paths::HeldRecheck>,
    /// The span of the call being checked when its function or receiver has effects
    /// (`getf()(...t)`): a spread's hidden temporary would run before it (`spread_args`).
    pub callee_effects: Option<Span>,
    /// The span of the callback arrow of a timer call (`setTimeout(() => …, ms)`) that is not
    /// `async`: it is checked as an async arrow (`expr/timer_task.rs`).
    pub void_task: Option<Span>,
    /// An arrow whose rest parameter is checked as an ordinary last parameter holding the
    /// packed arguments (`expr/rest_shapes.rs`).
    pub packed_rest: Option<Span>,
    /// A timer's callback that is a function value, not an arrow (`expr/callback.rs`).
    pub task_callback: Option<Span>,
    /// The handler arrow of a server call (`serve`) that is not `async`: checked as an async
    /// arrow (`expr/callback.rs`).
    pub thread_task: Option<Span>,
    /// The handler of a server call that is a function value, not an arrow.
    pub thread_callback: Option<Span>,
    /// An arrow checked as `async` by `thread_arrow`: its expression body is awaited when it is
    /// a promise.
    pub await_body: Option<Span>,
    /// The wrapper arrow `serve`'s adapter makes for a handler function value: the async
    /// closure it becomes is recorded in `Ctx::thread_adapters`.
    pub thread_adapter: Option<Span>,
    /// The member of a union of function types each arrow (by span) was typed by, for the
    /// members it was tried against and the types of the locals it names (`expr/closure.rs`).
    pub member_choices: FxHashMap<(Span, Vec<TyId>, Vec<TyId>), TyId>,
    /// What scans of each arrow's syntax found (`arrow_scans`).
    pub arrow_scans: arrow_scans::ArrowScans,
    /// Checking an expression outside any body (a field initializer, a parameter default, a
    /// module-level constant): it has no frame to hold temporary locals (`driver::detached`).
    pub detached: bool,
    /// The arrow (by span) that an adapter outside any body calls in place, and the function
    /// type it is checked as (`callback::immediate_wrapper`).
    pub immediate_hint: Option<(Span, TyId)>,
    /// The call about to be checked is `new Map(...)` / `new Set(...)`: an iterable argument
    /// for its array parameter is collected into an array (`consume.rs`).
    pub collect_iterable_args: bool,
    /// The call about to be checked is a weak operation of the prelude's `WeakMap`, `WeakSet`
    /// or `WeakRef`: its first argument is a weak key (`dynamic.rs` `weak_key_coerce`).
    pub weak_key_args: bool,
    /// Checking the `Object.keys(x)` of a plain value inside the typed proxy hooks (`proxy.rs`).
    pub proxy_keys_inner: bool,
    /// The explicit type arguments of the call about to be checked (at the first span): the
    /// slot each fills and its span, where a type bound it breaks is reported (`tsc` TS2344).
    pub explicit_type_spans: Option<(Span, Vec<(usize, Span)>)>,
    /// Where a value is given for an optional parameter (or one with a default) or an optional
    /// field: an `undefined` there is a limit of this phase of #930, not a type error
    /// (`expr/lit.rs` `optional_slot_error`).
    pub optional_slots: Vec<(Span, OptionalSlot)>,
    /// Reads of locals with a refused fact (`closure_assigned`), for notes on errors there.
    pub refused_reads: Vec<(Span, closure_assigned::Refused)>,
    /// A variable read that a null or truthiness test makes (`if (x)`, `x == null`): it reads
    /// the variable as it is, without the check `checked_narrowed_read` adds.
    pub raw_read: Option<Span>,
    /// Locals declared from integer literals and their uses (`literal_locals`).
    pub literal: literal_locals::LiteralLocals,
    /// The span of the JSX element whose template may be its string (`expr/jsx/list_fold.rs`),
    /// and whether it was.
    pub jsx_list_fold: Option<Span>,
    pub jsx_list_folded: bool,
    /// A list is being checked for a fold: lists inside it are not tried (each try may check
    /// its rows twice).
    pub jsx_list_trial: bool,
    /// Calls of methods returning the polymorphic `this` on `this` (`expr/this_type.rs`): their
    /// result is `this` itself, which such a method may return.
    pub this_calls: rustc_hash::FxHashSet<Span>,
    /// The member of a recursive closure group whose arrow is about to be checked: group,
    /// member and its definition.
    pub pending_member: Option<(usize, usize, DefId)>,
    /// Checking the body of an interface adapter's method (`expr/iface_adapt.rs`): an `unknown`
    /// converts to a typed value checked at run time, and a function value to another function
    /// type through an adapting arrow.
    pub adapting: bool,
}

impl<'a, 'm> FnCx<'a, 'm> {
    pub fn new(cx: &'a mut Ctx<'m>, module: usize, env: TyEnv, frame: Frame) -> Self {
        FnCx {
            cx,
            module,
            env,
            bounds: vec![],
            constraints: vec![],
            fn_name: String::new(),
            owner: None,
            body_def: None,
            static_this: None,
            enclosing_locals: vec![],
            generic_arrow: false,
            f: frame,
            outer: vec![],
            direct_await: None,
            std_callback: None,
            literal_slot: None,
            call_temps: vec![],
            held_recheck: Vec::new(),
            callee_effects: None,
            void_task: None,
            packed_rest: None,
            task_callback: None,
            thread_task: None,
            thread_callback: None,
            await_body: None,
            thread_adapter: None,
            member_choices: FxHashMap::default(),
            arrow_scans: Default::default(),
            detached: false,
            immediate_hint: None,
            collect_iterable_args: false,
            weak_key_args: false,
            proxy_keys_inner: false,
            explicit_type_spans: None,
            optional_slots: vec![],
            refused_reads: vec![],
            raw_read: None,
            literal: Default::default(),
            jsx_list_fold: None,
            jsx_list_folded: false,
            jsx_list_trial: false,
            this_calls: Default::default(),
            pending_member: None,
            adapting: false,
        }
    }

    pub fn mk(&self, kind: hir::ExprKind, ty: TyId, span: Span) -> hir::Expr {
        hir::Expr { kind, ty, span }
    }

    /// Error-typed placeholder expression.
    pub fn error_expr(&self, span: Span) -> hir::Expr {
        self.mk(hir::ExprKind::Lit(hir::Lit::Unit), self.cx.ty.error, span)
    }

    pub fn unit_expr(&self, span: Span) -> hir::Expr {
        self.mk(hir::ExprKind::Lit(hir::Lit::Unit), self.cx.ty.unit, span)
    }

    pub fn local_ty(&self, l: LocalId) -> TyId {
        self.f.locals[l.0 as usize].ty
    }

    pub fn local_kind(&self, l: LocalId) -> LocalKind {
        self.f.kinds[l.0 as usize]
    }

    /// Use mode of a value of type `ty` for the given consumer (places written or mutably
    /// borrowed are `BorrowMut` even for Copy types).
    pub fn use_mode(&mut self, ty: TyId, want: Want) -> UseMode {
        if want == Want::BorrowMut {
            UseMode::BorrowMut
        } else if self.cx.is_copy(ty) {
            UseMode::Copy
        } else {
            match want {
                Want::Move => UseMode::Move,
                Want::Borrow => UseMode::Borrow,
                Want::BorrowMut => UseMode::BorrowMut,
            }
        }
    }

    /// The loop / `switch` entry a `break` or `continue` targets (unlabeled: the innermost
    /// entry for `break`, the innermost loop for `continue`).
    pub fn loop_target(
        &mut self,
        label: Option<&velt_syntax::ast::Ident>,
        what: &str,
        span: Span,
    ) -> Option<usize> {
        let is_continue = what == "continue";
        let found = match label {
            None => self
                .f
                .loops
                .iter()
                .rposition(|lp| !(is_continue && lp.is_switch)),
            Some(l) => {
                let found = self
                    .f
                    .loops
                    .iter()
                    .rposition(|lp| lp.label.as_deref() == Some(l.name.as_str()));
                if found.is_none() {
                    self.cx
                        .err(format!("use of undeclared label `{}`", l.name), l.span);
                    return None;
                }
                found
            }
        };
        let Some(i) = found else {
            let msg = if is_continue || self.f.loops.is_empty() {
                format!("`{what}` outside of a loop")
            } else {
                format!("`{what}` outside of a loop or `switch`")
            };
            self.cx.err(msg, span);
            return None;
        };
        if is_continue && self.f.loops[i].is_switch {
            self.cx.err("`continue` cannot target a `switch`", span);
            return None;
        }
        if self.f.finally_loops.is_some_and(|n| i < n) {
            self.cx.error(
                Diagnostic::error(format!("`{what}` cannot leave a `finally` block in a generator"), span)
                    .with_note("TypeScript allows this; Velt doesn't because the `finally` block also runs when the generator is closed early (`return()`, or dropping it), where the generator must finish; write the loop inside the `finally` block, or move the `finally` code out of the loop"),
            );
            return None;
        }
        Some(i)
    }

    /// Record something that may throw at this point (caught by the innermost `try`, else
    /// propagated by the function).
    pub fn throw_src(&mut self, s: ThrowSrc) {
        match self.f.tries.last_mut() {
            Some(t) => t.push(s),
            None => self.f.uncaught.push(s),
        }
    }
}
