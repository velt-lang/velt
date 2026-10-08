//! Module constants initialized by a call (`const Counter = component(...)`, #383). Such a
//! constant is not stored: like every module constant, its initializer is evaluated at each use
//! (VIR `global()`). That matches TypeScript's evaluate-once only when the call is pure (no
//! effects, bounded, cannot throw) and its result is a value, so this module checks both, and
//! rejects comparing such a constant by identity, the one place where "a new value at each use"
//! would show.

use std::collections::HashMap;

use velt_common::{Diagnostic, Span};

use crate::ctx::Ctx;
use crate::defs::{BodyState, FnKind, ThrowSrc};
use crate::hir::{
    self, AdtKind, Callee, DefId, Expr, ExprKind as H, Intrinsic, StmtKind as S, TyId, TyKind,
};
use crate::visit::{self, VisitMut};

/// Why a function cannot initialize a module constant: what it does, where, and the calls that
/// lead there from the function the initializer calls.
#[derive(Clone, Debug)]
pub(crate) struct Impure {
    what: String,
    span: Span,
    via: Vec<String>,
}

/// Memoized purity of functions (`None` while being decided: a call back into it is recursion).
pub(crate) type PurityMemo = HashMap<DefId, Option<Result<(), Impure>>>;

/// Check the initializer of module constant `name` (type `ty`): constant expressions, and calls
/// of pure named functions with constant arguments.
pub(crate) fn check_init(cx: &mut Ctx, name: &str, init: &Expr, ty: TyId) {
    if !init_ok(cx, name, init) || !has_call(init) {
        return;
    }
    if let Some(what) = reference_part(cx, ty, 0) {
        cx.error(
            Diagnostic::error(
                format!(
                    "module constant `{name}` has type `{}` ({what}): initialized by a call, it would be a new object at each use",
                    cx.display(ty)
                ),
                init.span,
            )
            .with_note("module constants are evaluated at each use; only values (numbers, strings, structs, tuples, enums and functions) can come from a call")
            .with_note("create it in `main` and pass it where it is needed"),
        );
    }
}

/// Does `init` compute a new value at each use (a call, a closure or a function reference), so
/// that a use by value is another reference to it and identity shows?
pub(crate) fn has_call(e: &Expr) -> bool {
    match &e.kind {
        H::Call { .. } | H::Closure(_) | H::FnRef(..) => true,
        H::Unary { expr, .. } | H::Cast(expr) | H::WrapSome(expr) | H::Upcast(expr) => {
            has_call(expr)
        }
        H::Binary { lhs, rhs, .. } | H::Logical { lhs, rhs, .. } => has_call(lhs) || has_call(rhs),
        H::If { cond, then, els } => has_call(cond) || has_call(then) || has_call(els),
        H::AdtLit { fields: xs, .. } | H::Variant { args: xs, .. } | H::Tuple(xs) => {
            xs.iter().any(has_call)
        }
        _ => false,
    }
}

/// Report what in `e` is not allowed; false when something was reported.
fn init_ok(cx: &mut Ctx, name: &str, e: &Expr) -> bool {
    match &e.kind {
        H::Lit(_) | H::Global(_) | H::Closure(_) | H::FnRef(..) => true,
        H::Unary { expr, .. }
        | H::Cast(expr)
        | H::WrapSome(expr)
        | H::Upcast(expr)
        | H::Field { base: expr, .. } => init_ok(cx, name, expr),
        H::Binary { lhs, rhs, .. } | H::Logical { lhs, rhs, .. } => {
            init_ok(cx, name, lhs) && init_ok(cx, name, rhs)
        }
        H::If { cond, then, els } => {
            init_ok(cx, name, cond) && init_ok(cx, name, then) && init_ok(cx, name, els)
        }
        H::AdtLit { fields: xs, .. } | H::Variant { args: xs, .. } | H::Tuple(xs) => {
            xs.iter().all(|x| init_ok(cx, name, x))
        }
        H::Call {
            callee: Callee::Def(f, _),
            args,
        } => call_ok(cx, name, *f, args, e.span),
        H::Call {
            callee: Callee::Intrinsic(i),
            args,
        } => match effect_of(*i) {
            None => args.iter().all(|x| init_ok(cx, name, x)),
            Some(what) => {
                cx.error(
                    Diagnostic::error(
                        format!("cannot initialize module constant `{name}`: it {what}"),
                        e.span,
                    )
                    .with_note(fix_note(name)),
                );
                false
            }
        },
        H::Call {
            callee: Callee::Indirect(_),
            ..
        } => {
            cx.error(
                Diagnostic::error(
                    format!("cannot initialize module constant `{name}` from a function value: only named functions can be called here"),
                    e.span,
                )
                .with_note(fix_note(name)),
            );
            false
        }
        _ => {
            cx.error(
                Diagnostic::error(
                    "module-level constants must be constant expressions or pure calls",
                    e.span,
                )
                .with_note("use literals, struct literals of constants, other module constants, or a call of a named function without effects whose arguments are constants or closures")
                .with_note(fix_note(name)),
            );
            false
        }
    }
}

fn fix_note(name: &str) -> String {
    format!("compute it in `main` and pass it on, or make `{name}` a function: `function {name}() {{ return ...; }}`")
}

/// A call `f(args)` in an initializer: constant arguments, pure `f`.
fn call_ok(cx: &mut Ctx, name: &str, f: DefId, args: &[Expr], span: Span) -> bool {
    let fname = short_name(cx, f);
    for (i, a) in args.iter().enumerate() {
        if !matches!(a.kind, H::Call { .. }) && !arg_ok(a) {
            cx.error(
                Diagnostic::error(
                    format!("argument {} of `{fname}` is not constant: module constants take constant expressions, other module constants and closures", i + 1),
                    a.span,
                )
                .with_note(fix_note(name)),
            );
            return false;
        }
        if !init_ok(cx, name, a) {
            return false;
        }
    }
    match purity(cx, f) {
        Ok(()) => true,
        Err(imp) => {
            let mut d = Diagnostic::error(
                format!(
                    "`{fname}` cannot initialize module constant `{name}`: it {}",
                    imp.what
                ),
                span,
            )
            .with_label(imp.span, imp.what.clone());
            if !imp.via.is_empty() {
                let chain = std::iter::once(fname.clone())
                    .chain(imp.via.iter().cloned())
                    .map(|n| format!("`{n}`"))
                    .collect::<Vec<_>>()
                    .join(" -> ");
                d = d.with_note(format!("through {chain}"));
            }
            cx.error(
                d.with_note("module constants are evaluated at each use, so a call that initializes one must have no effects, no loops or recursion, and must not throw or await")
                    .with_note(fix_note(name)),
            );
            false
        }
    }
}

/// A constant argument: constant expressions, module constants, function references, closures
/// (a module-level closure captures nothing), and nested calls (their purity is `init_ok`'s).
fn arg_ok(e: &Expr) -> bool {
    match &e.kind {
        H::Lit(_) | H::Global(_) | H::FnRef(..) | H::Closure(_) => true,
        H::Call {
            callee: Callee::Def(..),
            args,
        } => args.iter().all(arg_ok),
        H::Call {
            callee: Callee::Intrinsic(Intrinsic::Share),
            args,
        } => args.iter().all(arg_ok),
        H::Unary { expr, .. }
        | H::Cast(expr)
        | H::WrapSome(expr)
        | H::Upcast(expr)
        | H::Field { base: expr, .. } => arg_ok(expr),
        H::Binary { lhs, rhs, .. } | H::Logical { lhs, rhs, .. } => arg_ok(lhs) && arg_ok(rhs),
        H::If { cond, then, els } => arg_ok(cond) && arg_ok(then) && arg_ok(els),
        H::AdtLit { fields: xs, .. } | H::Variant { args: xs, .. } | H::Tuple(xs) => {
            xs.iter().all(arg_ok)
        }
        _ => false,
    }
}

/// The name of a function as written (without its module path).
fn short_name(cx: &Ctx, f: DefId) -> String {
    let n = &cx.fn_info(f).name;
    n.rsplit("::").next().unwrap_or(n).to_string()
}

/// Is `f` pure (memoized)? Its body is checked first if it isn't yet.
pub(crate) fn purity(cx: &mut Ctx, f: DefId) -> Result<(), Impure> {
    match cx.pure_fns.get(&f) {
        Some(Some(r)) => return r.clone(),
        Some(None) => {
            return Err(Impure {
                what: "calls itself".into(),
                span: cx.fn_info(f).name_span,
                via: vec![],
            })
        }
        None => {}
    }
    cx.pure_fns.insert(f, None);
    let r = decide(cx, f);
    cx.pure_fns.insert(f, Some(r.clone()));
    r
}

fn decide(cx: &mut Ctx, f: DefId) -> Result<(), Impure> {
    let info = cx.fn_info(f);
    let at = |what: &str, span| {
        Err(Impure {
            what: what.into(),
            span,
            via: vec![],
        })
    };
    let name_span = info.name_span;
    if info.kind == FnKind::Extern {
        return at(
            "calls an external function, which may have effects",
            name_span,
        );
    }
    if info.is_async || info.is_async_gen {
        return at("is async", name_span);
    }
    if info.is_generator {
        return at("is a generator", name_span);
    }
    if info.state == BodyState::InProgress {
        return at("depends on the constant it initializes", name_span);
    }
    super::driver::ensure_body(cx, f);
    let info = cx.fn_info(f);
    for s in &info.throw_srcs {
        match s {
            ThrowSrc::Direct(t, span) if *t != cx.ty.never => return at("may throw", *span),
            ThrowSrc::Slot { span, .. } => return at("calls an interface method", *span),
            _ => {}
        }
    }
    let Some(hir::Def::Fn(def)) = cx.defs[f.0 as usize].as_ref() else {
        return Ok(());
    };
    let mut body = def.body.block.clone();
    let mut walk = Walk {
        found: None,
        calls: vec![],
    };
    visit::block(&mut body, &mut walk);
    if let Some(imp) = walk.found {
        return Err(imp);
    }
    for (g, span) in walk.calls {
        if let Err(mut imp) = purity(cx, g) {
            // A reason about the callee as a whole points at the call.
            if imp.via.is_empty() && imp.span == cx.fn_info(g).name_span {
                imp.span = span;
            }
            if g != f && cx.fn_info(g).kind != FnKind::Extern {
                imp.via.insert(0, short_name(cx, g));
            }
            return Err(imp);
        }
    }
    Ok(())
}

/// The first effect found in a body, and the named functions it calls.
struct Walk {
    found: Option<Impure>,
    calls: Vec<(DefId, Span)>,
}

impl Walk {
    fn set(&mut self, what: &str, span: Span) {
        if self.found.is_none() {
            self.found = Some(Impure {
                what: what.into(),
                span,
                via: vec![],
            });
        }
    }
}

impl VisitMut for Walk {
    fn stmt(&mut self, s: &mut hir::Stmt) {
        if matches!(s.kind, S::While { .. } | S::ForOf { .. }) {
            self.set("has a loop", s.span);
        }
    }

    fn expr(&mut self, e: &mut Expr) {
        match &e.kind {
            H::Call { callee, .. } => match callee {
                Callee::Def(g, _) => self.calls.push((*g, e.span)),
                Callee::Indirect(_) => self.set("calls a function value", e.span),
                Callee::Virtual { .. } | Callee::Dyn { .. } | Callee::ParamMethod { .. } => {
                    self.set("calls a method chosen at run time", e.span)
                }
                Callee::Intrinsic(i) => {
                    if let Some(what) = effect_of(*i) {
                        self.set(&what, e.span);
                    }
                }
            },
            H::Await(_) => self.set("awaits", e.span),
            H::Throw(_) => self.set("may throw", e.span),
            _ => {}
        }
    }
}

/// What an intrinsic does that a module constant's initializer may not (`None`: allowed).
fn effect_of(i: Intrinsic) -> Option<String> {
    use Intrinsic as I;
    let name = match i {
        I::Print => "console.log",
        I::PrintErr => "console.error",
        I::Exit => "process.exit",
        I::DateNow => "Date.now",
        I::PerfNow => "performance.now",
        I::SharedNew | I::SharedAdd | I::SharedGet | I::SharedSet => "shared",
        I::MutexNew | I::MutexWith => "Mutex",
        I::Spawn | I::SpawnHandled => "spawn",
        I::JsonParse => "JSON.parse",
        I::ToString
        | I::StrConcat
        | I::StrLen
        | I::StrCharCodeAt
        | I::Panic
        | I::ArrayWithCapacity
        | I::ArrayLen
        | I::ArrayPush
        | I::ArrayPop
        | I::ArraySwap
        | I::ArrayRemove
        | I::ArrayTruncate
        | I::Hash
        | I::Eq
        | I::Same
        | I::Clone
        | I::Share
        | I::FieldAbsent
        | I::FieldPresent
        | I::NeedsTransfer
        | I::NeedsDrop
        | I::MayAlias
        | I::FnCapturesNothing
        | I::Sqrt
        | I::Floor
        | I::Ceil
        | I::Round
        | I::Trunc
        | I::FAbs
        | I::JsonStringify
        | I::SourceLocation => return None,
        _ => return Some("uses an operation with effects".into()),
    };
    Some(format!("calls `{name}`, which has effects"))
}

/// The part of `t` that is a reference (an object that would be new at each use), if any.
fn reference_part(cx: &Ctx, t: TyId, depth: u32) -> Option<&'static str> {
    if depth > 16 {
        return None;
    }
    match cx.ty.kind(t).clone() {
        TyKind::Array(_) => Some("an array"),
        TyKind::Map(..) => Some("a map"),
        TyKind::Shared(_) => Some("a shared value"),
        TyKind::Promise(..) => Some("a promise"),
        TyKind::Dyn(..) => Some("an interface"),
        TyKind::Option(x) => reference_part(cx, x, depth + 1),
        TyKind::Tuple(xs) => xs.iter().find_map(|x| reference_part(cx, *x, depth + 1)),
        TyKind::Adt(d, args) => {
            if let Some(a) = cx.adt(d) {
                if a.kind == AdtKind::Class {
                    return Some("a class");
                }
                let fields: Vec<TyId> = a.fields.iter().map(|f| f.ty).collect();
                return fields
                    .iter()
                    .chain(args.iter())
                    .find_map(|x| reference_part(cx, *x, depth + 1));
            }
            let e = cx.enum_info(d)?;
            let payloads: Vec<TyId> = e.variants.iter().flat_map(|v| v.payload.clone()).collect();
            payloads
                .iter()
                .chain(args.iter())
                .find_map(|x| reference_part(cx, *x, depth + 1))
        }
        _ => None,
    }
}

/// Does `===` compare values of `t` by identity (in part)?
fn identity_compared(cx: &Ctx, t: TyId, depth: u32) -> bool {
    if depth > 16 {
        return false;
    }
    match cx.ty.kind(t).clone() {
        TyKind::FnPtr { .. } | TyKind::Closure(_) | TyKind::Array(_) | TyKind::Map(..) => true,
        TyKind::Dyn(..) | TyKind::Shared(_) | TyKind::Promise(..) => true,
        TyKind::Adt(d, _) => cx.adt(d).is_some(),
        TyKind::Option(x) => identity_compared(cx, x, depth + 1),
        TyKind::Tuple(xs) => xs.iter().any(|x| identity_compared(cx, *x, depth + 1)),
        _ => false,
    }
}

/// `===` / `!==` with an operand that is directly a module constant compared by identity: the
/// constant is a new value at each use, so the comparison would always be false.
pub(crate) fn check_identity(cx: &mut Ctx, operands: [&Expr; 2]) {
    for e in operands {
        let mut x = e;
        while let H::Upcast(i) | H::WrapSome(i) | H::Cast(i) = &x.kind {
            x = i;
        }
        let H::Global(d) = x.kind else { continue };
        let Some(g) = cx.global(d) else { continue };
        let fresh = g
            .init
            .as_ref()
            .is_some_and(|i| !matches!(i.kind, H::Lit(_)));
        if !fresh || !identity_compared(cx, g.ty, 0) {
            continue;
        }
        let name = g.name.clone();
        cx.error(
            Diagnostic::error(
                format!("`{name}` is evaluated at each use, so comparing it by identity is always false"),
                x.span,
            )
            .with_note("module constants are not stored: each use of one computes a new value")
            .with_note(format!("copy it into a local first (`const c = {name};`) and compare that, or compare a field that identifies it")),
        );
        return;
    }
}
