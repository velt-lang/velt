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
mod checked_copies;
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
mod forward_consts;
mod frame;
mod gated;
mod generators;
mod global_narrow;
mod index_keys;
mod key_narrow;
mod late_consts;
pub(crate) mod lazy_std;
pub(crate) mod literal_hosts;
pub(crate) mod literal_locals;
mod type_guards;
pub(crate) use generators::{weak_copy_error, GenCopy, LOCK_NOTE};
mod chain_facts;
mod literal_patterns;
mod locals;
mod loops;
pub(crate) mod moved_const;
pub(crate) mod narrow;
mod narrow_generic;
mod nested_pattern;
mod object_rest;
mod param_patterns;
mod pattern;
mod pattern_defaults;
pub(crate) mod places;
mod predicate_unrelated;
mod predicates;
mod property_pattern;
pub(crate) mod pure_init;
mod readonly_arrays;
pub(crate) mod recheck;
pub(crate) mod recursion;
pub(crate) mod returns;
mod stmt;
pub(crate) use readonly_arrays::is_readonly_array;
pub(crate) use readonly_arrays::{is_readonly_type, NUMBER_RECORD};
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
pub(crate) use driver::{
    check_bodies, ensure_body, ensure_global, field_defaults, holds_promise_by_value, printable,
    promise_in_state,
};
pub(crate) use expr::REPORTED;
pub(crate) use frame::Frame;
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
    /// Union locals that hold the member they are narrowed to because it was assigned to them
    /// (never a view, `crate::union_views`).
    pub held: Vec<LocalId>,
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
    /// That narrowing: it holds in the whole body, also past branches that started before the
    /// capture was made (`FnCx::restore_narrowing`).
    pub start: locals::Narrowing,
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
    /// In a static method of an `extend` block of a primitive type: the type's name and the
    /// type, what `this` is (`expr::primitive_statics`).
    pub this_primitive: Option<(&'static str, TyId)>,
    /// Locals of the functions enclosing a nested declaration (see `collect::nested`).
    pub enclosing_locals: Vec<String>,
    /// The body is a local generic arrow function (checked as a nested function).
    pub generic_arrow: bool,
    pub f: Frame,
    /// Enclosing frames of the closure being checked (innermost last).
    pub outer: Vec<Frame>,
    /// The arrow being checked is an argument of a JS API function called from user code
    /// (`numbers::is_js_api`): per parameter, whether the signature declares it an integer (an
    /// index), which makes it a number in the arrow's body when unannotated.
    pub std_callback: Option<Vec<bool>>,
    /// An arrow whose parameters without a type are `any` (`dynamic_member.rs`).
    pub any_callback: Option<Span>,
    /// Operands of reflective operations (`Object.keys`, `in`, `Object.hasOwn`, spreads, ...),
    /// whose class instance brands are checked as their classes (`brand_members.rs`).
    pub instance_args: rustc_hash::FxHashSet<velt_syntax::ast::NodeId>,
    /// The arrow checked next is a `then` / `catch` / `finally` callback: its result type is
    /// its own (`FnState::never_end`).
    pub promise_callback: bool,
    /// The value being checked fills slot `.1` of interface `.0` in an object literal implementing
    /// it, and is written at `.2` (`expr/iface_object.rs`): a closure written there records
    /// what it throws for the slot before its error type is computed.
    pub literal_slot: Option<(DefId, u32, Span)>,
    /// The object-literal method written at `.0` takes the object it is called on, of type
    /// `.1`, as its `this` parameter (`expr/literal_this.rs`).
    pub method_this: Option<(Span, TyId)>,
    /// The property read written at this span is the value of an object-literal property
    /// (`expr/literal_this.rs` `stored_value`).
    pub stored_prop: Option<Span>,
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
    /// The call about to be checked is of a `declare system function`: its byte buffer
    /// parameters take views too (`system_args.rs`).
    pub system_args: bool,
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
    /// Reads that need the payload of a narrowed module variable, field path or checked copy:
    /// receivers, callees, operands of arithmetic and comparisons, spreads (`global_narrow`).
    pub payload_spans: Vec<Span>,
    /// Reads whose value is used as it is (`"a" + x`): not taken as a payload.
    pub as_is_spans: Vec<Span>,
    /// A field read that is the operand of a type test (`typeof o.f`, `o.f instanceof C`,
    /// `o.f === lit`): it reads the field as narrowed, without converting it to the union of the
    /// members left (`field_narrow`).
    pub field_test: Option<Span>,
    /// A narrowed field read is being converted to the union of the members left
    /// (`field_narrow`): a member ruled out means the field changed since the test.
    pub field_retag: bool,
    /// The expression being checked is the object of a member read (`body::expr::union_view`).
    pub view_receiver: bool,
    /// The arrow being checked as the operand of a type assertion (`callables`).
    pub asserted_fn: Option<Span>,
    /// The operand of a type assertion: a narrowed `any` / `unknown` there reads as it is.
    pub dynamic_raw: Option<Span>,
    /// The types of variables declared before their declaration, by name (`forward_consts`).
    pub forward_tys: rustc_hash::FxHashMap<Span, TyId>,
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
            this_primitive: None,
            enclosing_locals: vec![],
            generic_arrow: false,
            asserted_fn: None,
            dynamic_raw: None,
            forward_tys: Default::default(),
            f: frame,
            outer: vec![],
            std_callback: None,
            any_callback: None,
            instance_args: Default::default(),
            promise_callback: false,
            literal_slot: None,
            method_this: None,
            stored_prop: None,
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
            system_args: false,
            proxy_keys_inner: false,
            explicit_type_spans: None,
            optional_slots: vec![],
            refused_reads: vec![],
            raw_read: None,
            payload_spans: vec![],
            as_is_spans: vec![],
            field_test: None,
            field_retag: false,
            view_receiver: false,
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
