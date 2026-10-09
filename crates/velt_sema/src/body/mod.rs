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

mod assigned;
mod closure_assigned;
mod const_borrow;
mod consume;
mod ctor;
mod defaults;
mod driver;
pub(crate) mod expr;
mod field_narrow;
mod for_await;
mod for_iter;
mod generators;
pub(crate) mod literal_locals;
pub(crate) use generators::GenCopy;
mod locals;
mod loops;
pub(crate) mod mentions;
pub(crate) mod narrow;
mod nested_pattern;
mod pattern;
mod pattern_defaults;
pub(crate) mod places;
mod property_pattern;
pub(crate) mod pure_init;
pub(crate) mod recheck;
pub(crate) mod recursion;
pub(crate) mod returns;
mod stmt;
pub(crate) mod switch;
mod untyped_let;
mod using;

use std::collections::HashMap;

use velt_common::{Diagnostic, Span};

use crate::ctx::Ctx;
use crate::defs::{Bound, FnKind, ThrowSrc};
use crate::hir::{self, DefId, LocalDef, LocalId, TyId, UseMode};
use crate::resolve::TyEnv;

pub(crate) use defaults::param_defaults;
pub(crate) use driver::{check_bodies, ensure_body, field_defaults, printable};

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
    pub names: HashMap<String, LocalId>,
    /// Option locals narrowed to their payload inside this scope.
    pub narrowed: Vec<LocalId>,
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

/// Per-function checking state; closures push a new frame (the enclosing one is saved).
#[derive(Clone)]
pub(crate) struct Frame {
    pub kind: FnKind,
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
    /// Body of a named generator function expression: its name (not in scope there).
    pub fn_expr_name: Option<String>,
    /// Nesting depth of the `finally` blocks being checked (`yield` is not allowed in them).
    pub finally_depth: u32,
    /// In a generator's `finally` block: the loop stack's length when it was entered (a
    /// `break`/`continue` there cannot target a loop outside it).
    pub finally_loops: Option<usize>,
    /// Spans (`lo`, `hi`) of the `yield`s about to be checked whose value is unused: those
    /// that are, or end, an expression statement (`generators.rs`, `stmt_yields`).
    pub stmt_yields: std::collections::HashSet<(u32, u32)>,
    /// See `FnInfo::soft_moves`.
    pub soft_moves: Vec<Span>,
    /// `using` variables passed to an async call (receiver or argument), not yet soft moves:
    /// only a directly awaited call may share one (`expr::tasks`, `using_share`).
    pub using_shares: Vec<(Span, LocalId)>,
    /// The `using` locals declared `await using`.
    pub await_using: std::collections::HashSet<LocalId>,
    /// `const f = (…) => …`: the closure each such local holds, whose parameter defaults a
    /// call `f(…)` fills in.
    pub closure_consts: std::collections::HashMap<LocalId, DefId>,
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
    pub const_refs: std::collections::HashSet<LocalId>,
    /// Tokens of field paths tested by `instanceof` that are not narrowed (a field on the path
    /// is not `readonly`), and the reads of them since (`field_narrow`, for error notes).
    pub mutable_tests: Vec<LocalId>,
    pub unnarrowed_reads: Vec<Span>,
    /// Names of the variables that closures created in this function's body assign, with
    /// where (`closure_assigned`): they are not narrowed.
    pub closure_assigned: HashMap<String, Span>,
    /// A callback of the JS API returning a number: an integer it returns converts
    /// (`returns::returned`).
    pub int_returns_number: bool,
    /// `const k = "a"` without a type: the literal each such local holds, which a `case k:`
    /// selects like the literal itself (TypeScript gives the constant the literal type).
    pub const_lits: HashMap<LocalId, velt_syntax::ast::SignedLit>,
    /// `let x;` without a type or initializer, not assigned yet: where each is declared. The
    /// first assignment gives it its type (`untyped_let`).
    pub untyped_lets: HashMap<LocalId, Span>,
}

impl Frame {
    pub fn new(kind: FnKind, ret: Option<TyId>) -> Self {
        Frame {
            kind,
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
            fn_expr_name: None,
            finally_depth: 0,
            finally_loops: None,
            stmt_yields: Default::default(),
            soft_moves: vec![],
            using_shares: vec![],
            await_using: Default::default(),
            closure_consts: Default::default(),
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
            closure_assigned: HashMap::new(),
            int_returns_number: false,
            const_lits: HashMap::new(),
            untyped_lets: HashMap::new(),
        }
    }
}

pub(crate) struct FnCx<'a, 'm> {
    pub cx: &'a mut Ctx<'m>,
    pub module: usize,
    /// Generic params in scope (for annotations inside the body).
    pub env: TyEnv,
    pub bounds: Vec<Vec<Bound>>,
    /// Name of the enclosing top-level function (for closure names).
    pub fn_name: String,
    /// Type whose body is being checked (methods, constructors, defaults, field initializers):
    /// its `private` members are accessible.
    pub owner: Option<DefId>,
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
    /// The span of the callback arrow of a timer call (`setTimeout(() => …, ms)`) that is not
    /// `async`: it is checked as an async arrow (`expr/timer_task.rs`).
    pub void_task: Option<Span>,
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
    /// The member of a union of function types each arrow (by span) was typed by, for the
    /// members it was tried against (`expr/closure.rs`).
    pub member_choices: HashMap<(Span, Vec<TyId>), TyId>,
    /// Parameter names of the arrows whose union member is being tried (`member_choices` is
    /// not used for an arrow that mentions one).
    pub trial_params: Vec<String>,
    /// Checking an expression outside any body (a field initializer, a parameter default, a
    /// module-level constant): it has no frame to hold temporary locals (`driver::detached`).
    pub detached: bool,
    /// The call about to be checked is `new Map(...)` / `new Set(...)`: an iterable argument
    /// for its array parameter is collected into an array (`consume.rs`).
    pub collect_iterable_args: bool,
    /// Reads of locals with a refused fact (`closure_assigned`), for notes on errors there.
    pub refused_reads: Vec<(Span, closure_assigned::Refused)>,
    /// Locals declared from integer literals and their uses (`literal_locals`).
    pub literal: literal_locals::LiteralLocals,
    /// The span of the JSX element whose template may be its string (`expr/jsx/list_fold.rs`),
    /// and whether it was.
    pub jsx_list_fold: Option<Span>,
    pub jsx_list_folded: bool,
    /// A list is being checked for a fold: lists inside it are not tried (each try may check
    /// its rows twice).
    pub jsx_list_trial: bool,
}

impl<'a, 'm> FnCx<'a, 'm> {
    pub fn new(cx: &'a mut Ctx<'m>, module: usize, env: TyEnv, frame: Frame) -> Self {
        FnCx {
            cx,
            module,
            env,
            bounds: vec![],
            fn_name: String::new(),
            owner: None,
            enclosing_locals: vec![],
            generic_arrow: false,
            f: frame,
            outer: vec![],
            direct_await: None,
            std_callback: None,
            void_task: None,
            task_callback: None,
            thread_task: None,
            thread_callback: None,
            await_body: None,
            member_choices: HashMap::new(),
            trial_params: vec![],
            detached: false,
            collect_iterable_args: false,
            refused_reads: vec![],
            literal: Default::default(),
            jsx_list_fold: None,
            jsx_list_folded: false,
            jsx_list_trial: false,
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
