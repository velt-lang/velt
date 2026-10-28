//! The per-function checking state ([`Frame`]): what one function or closure body being checked
//! knows (its locals, scopes, loops, captures, result type and the bookkeeping of its returns,
//! `await`s and `finally` blocks). Closures push a new frame; the enclosing one is saved.

use super::*;

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
    /// The `typeIs<A, B>()` tests that hold where the code being checked runs: the `then`
    /// branches of `if (typeIs<A, B>())` (`body::type_guards`), innermost last.
    pub type_guards: Vec<(TyId, TyId)>,
    pub locals: Vec<LocalDef>,
    pub kinds: Vec<LocalKind>,
    pub scopes: Vec<Scope>,
    pub loops: Vec<LoopCx>,
    /// Loops and `switch`es entered so far (numbers synthesized labels).
    pub loop_count: u32,
    /// Declared/expected return type; `None` while it is inferred from the body's `return`s
    /// (recorded in `returns`).
    pub ret: Option<TyId>,
    /// Where a closure's result goes (`returns::ReturnSite`).
    pub return_site: returns::ReturnSite,
    pub returns: returns::Returns,
    pub captures: Vec<CaptureCx>,
    pub escaping: bool,
    /// An arrow function whose `void` result comes from the expected function type, not from an
    /// annotation: as in TypeScript, the value its body or a `return` gives is evaluated and
    /// dropped (`(s: string) => void` takes `(s) => out.push(s)` whatever `push` returns).
    pub discards_value: bool,
    /// An arrow without a written result type asserted to a function type (`(() => …) as F`):
    /// a returned `any` / `unknown` converts to the result, checked (`returns::returned`).
    pub asserted_ret: bool,
    /// An arrow whose result type nothing expected fills (no function type is expected, or it
    /// is a promise callback): an end never reached and no `return` give `never`, as in
    /// TypeScript. Inferred for a type argument (`xs.map((x) => { throw e; })`) it stays `void`.
    pub never_end: bool,
    /// Body of an `async` function / arrow: `await` is allowed.
    pub is_async: bool,
    /// The program's top-level statements (the root file's generated `async main`): its
    /// `await`s tell the runtime where they wait (`expr/top_level_await.rs`); the top-level
    /// `await`s whose operand is being checked (innermost last).
    pub top_level: bool,
    pub top_level_outer: Vec<Span>,
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
    /// Class values pending as `const`s at the start of their blocks (`expr::ctor_locals`).
    pub class_lets: Vec<expr::ctor_locals::ClassLet>,
    /// The scopes of the statement lists being checked (`stmts_into`), outermost first.
    pub stmt_lists: Vec<usize>,
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
    /// Marks of locals: [`READONLY`] for one declared with a read-only array or collection type,
    /// which the body may not write to (`readonly_arrays.rs`), [`NUMBER_RECORD`] for one declared
    /// with a number-keyed record type (`expr::number_records`).
    pub local_marks: FxHashMap<LocalId, u8>,
    /// What the position of an assignment used as a value expects (`assign_value`): a
    /// `new C()` assigned through `any` with `??=`, `||=` or `&&=` takes its type arguments from
    /// it (`dynamic_write`).
    pub assign_hint: Option<TyId>,
    /// The read-only local a method call written `(x as T).m()` may change (`T` not read-only;
    /// `readonly_arrays.rs`).
    pub readonly_exempt: Option<LocalId>,
    /// A method call written `(x as T).m()` with `T` read-only (`readonly_arrays.rs`).
    pub readonly_cast_recv: bool,
    /// `const c = [2, "y"] as const` (literals of different types): its elements, which an
    /// array pattern taking `c` apart reads as the literal (`literal_patterns`).
    pub as_const_tuples: FxHashMap<LocalId, Vec<velt_syntax::ast::Expr>>,
    /// `const f = (…) => …`: the closure each such local holds, whose parameter defaults a
    /// call `f(…)` fills in.
    pub closure_consts: rustc_hash::FxHashMap<LocalId, DefId>,
    /// `const g: G = id`: the named generic function each such local holds, instantiated where
    /// the local is used as a function of fixed types (`expr::generic_conversions`).
    pub named_fns: rustc_hash::FxHashMap<LocalId, DefId>,
    /// The property writes after the declaration being checked, which a cast of a function to
    /// a callable object type there takes as members (`expr::callables`, #921).
    pub callable_writes: Option<expr::PendingWrites>,
    /// Variables made functions with properties by `Object.assign(f, { … })`, with the call's
    /// span (`expr::callables`).
    pub assign_targets: Vec<(LocalId, Span)>,
    /// Locals declared with a method read as a value (`const f = o.m`): a call that passes
    /// them another `this` is not supported yet (`call_apply`).
    pub method_values: rustc_hash::FxHashSet<LocalId>,
    /// `const` locals declared with a property read of a variable (`const f = (o as T).m`), with
    /// that variable: `f.call(o)` runs `f` on the object it keeps while nothing assigns `o`
    /// (`call_apply`).
    pub member_reads: rustc_hash::FxHashMap<LocalId, LocalId>,
    /// Throw sources of the enclosing `try` bodies (innermost last).
    pub tries: Vec<Vec<ThrowSrc>>,
    /// Each `catch` variable used only as `throw e`, with the throw sources of its `try` body:
    /// rethrowing it throws what they throw (`expr::errors`).
    pub caught: Vec<(LocalId, Vec<ThrowSrc>)>,
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
    /// Module variables that conditions narrow (`global_narrow`).
    pub global_tokens: Vec<global_narrow::GlobalToken>,
    /// `const`s bound by reference (`const_borrow`).
    pub const_refs: rustc_hash::FxHashSet<LocalId>,
    /// Tokens of field paths tested by `instanceof` that are not narrowed (a field on the path
    /// is not `readonly`), and the reads of them since (`field_narrow`, for error notes).
    pub mutable_tests: Vec<LocalId>,
    /// With whether the field is one of a module variable's (`global_narrow`).
    pub unnarrowed_reads: Vec<(Span, bool)>,
    /// Names of the variables that closures created in this function's body assign, with
    /// where (`closure_assigned`): they are not narrowed.
    pub closure_assigned: FxHashMap<String, Span>,
    /// Untyped locals initialized from a narrowed module variable or field path: nullable,
    /// with a null check fact whose reads are checked (`checked_copies`).
    pub checked_copies: rustc_hash::FxHashSet<LocalId>,
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
            type_guards: vec![],
            locals: vec![],
            kinds: vec![],
            scopes: vec![Scope::default()],
            loops: vec![],
            loop_count: 0,
            ret,
            return_site: Default::default(),
            returns: Default::default(),
            captures: vec![],
            escaping: false,
            discards_value: false,
            asserted_ret: false,
            never_end: false,
            is_async: false,
            top_level: false,
            top_level_outer: vec![],
            yield_ty: None,
            next_ty: None,
            catch_depth: 0,
            fn_expr_name: None,
            finally_depth: 0,
            adopted: vec![],
            class_lets: vec![],
            stmt_lists: vec![],
            adopt_which: None,
            adopt_stmt: false,
            finally_loops: None,
            stmt_yields: Default::default(),
            soft_moves: vec![],
            using_shares: vec![],
            await_using: Default::default(),
            local_marks: Default::default(),
            assign_hint: None,
            readonly_exempt: None,
            readonly_cast_recv: false,
            as_const_tuples: Default::default(),
            closure_consts: Default::default(),
            named_fns: Default::default(),
            callable_writes: None,
            assign_targets: vec![],
            method_values: Default::default(),
            member_reads: Default::default(),
            tries: vec![],
            caught: vec![],
            uncaught: vec![],
            super_ok: false,
            super_called: false,
            super_sites: vec![],
            super_silent: vec![],
            before_super: false,
            stmt_depth: 0,
            field_tokens: vec![],
            global_tokens: vec![],
            const_refs: Default::default(),
            mutable_tests: vec![],
            unnarrowed_reads: vec![],
            closure_assigned: FxHashMap::default(),
            checked_copies: Default::default(),
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
