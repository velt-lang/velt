//! After ownership inference: every move must come out of something owned. Reports moves out
//! of array elements, class-instance fields, borrowed parameters / `this`, `for...of` elements
//! and captured variables (each with a `.clone()` hint), and functions used as values whose
//! parameters are owned (a function value's ABI borrows).

use std::collections::{HashMap, HashSet};

use velt_common::{Diagnostic, Span};

use crate::body::LocalKind;
use crate::ctx::Ctx;
use crate::defs::BodyState;
use crate::hir::{
    Callee, Def, DefId, Expr, ExprKind as E, FnDef, Intrinsic, LocalDef, LocalId, PassMode, TyId,
    UseMode,
};
use crate::visit;

use super::soft;

pub(crate) fn validate_moves(cx: &mut Ctx) {
    let fns: Vec<DefId> = cx
        .fn_defs
        .iter()
        .copied()
        .filter(|d| cx.fn_info(*d).state == BodyState::Done)
        .collect();
    for d in fns {
        let Some(Def::Fn(mut f)) = cx.defs[d.0 as usize].take() else {
            continue;
        };
        let mut errors = vec![];
        let mut copied_captures = vec![];
        let mut deep_copies = vec![];
        let kinds = cx.fn_info(d).local_kinds.clone();
        let fixed = cx.fn_info(d).fixed_modes;
        let keeps_fn_params = cx.fn_info(d).keeps_fn_params;
        let soft: HashSet<Span> = cx.fn_info(d).soft_moves.iter().copied().collect();
        let shared = shared_captures_in(cx, &mut f.body.block);
        let v = Validator {
            cx,
            f: &f,
            kinds: &kinds,
            fixed,
            keeps_fn_params,
            shared: &shared,
        };
        let mut block = f.body.block.clone();
        visit::exprs_mut(&mut block, &mut |e: &mut Expr| {
            if soft.contains(&e.span) && soft::is_moved_place(e) {
                // A `using` variable keeps its value until the end of its block (except for its
                // `await using` cleanup call, which carries the declared name's span).
                if let E::Local(l, _) = e.kind {
                    let using = kinds.get(l.0 as usize) == Some(&LocalKind::Using);
                    if using && e.span != f.body.locals[l.0 as usize].span {
                        return soft::make_share(e);
                    }
                }
                let mut invalid = vec![];
                v.check(e, &mut invalid);
                if !invalid.is_empty() {
                    match super::fn_values::borrowed_fn_copy(
                        v.cx,
                        v.f,
                        v.kinds,
                        v.keeps_fn_params,
                        e,
                    ) {
                        Some(err) => errors.push(err),
                        // An async closure that may run on several threads at once (an
                        // http handler) copies what it captured: counts are not atomic. (An
                        // async generator and a local async closure stay on their task and
                        // share, like JS.)
                        None if v.f.is_async
                            && !v.f.is_generator
                            && !v.f.shares_captures
                            && v.captured(e) =>
                        {
                            deep_copies.push((e.span, e.ty));
                            soft::make_deep_copy(e)
                        }
                        None => soft::make_share(e),
                    }
                    return;
                }
            }
            if let (E::Closure(c), true) = (&e.kind, soft.contains(&e.span)) {
                let pinned = v.shared_captures_pinned(*c);
                if !pinned.is_empty() {
                    copied_captures.push((*c, pinned));
                }
            }
            v.check(e, &mut errors)
        });
        cx.diags.extend(errors);
        share_uncopyable(cx, &mut block, &deep_copies);
        f.body.block = block;
        cx.defs[d.0 as usize] = Some(Def::Fn(f));
        for (c, pinned) in copied_captures {
            super::shares::share_captures(cx, c, &pinned);
        }
    }
    fn_values(cx);
}

/// Of the captures an async closure deep-copies (`deep_copies`: the copy's span and type),
/// those owning a resource without `clone()` are shared instead (#122): a copy would release
/// the resource twice, and the call runs on the task that owns the closure. (An http handler
/// clones what its body consumes per request, and that copy panics on such a resource.)
fn share_uncopyable(cx: &mut Ctx, b: &mut crate::hir::Block, deep_copies: &[(Span, TyId)]) {
    let spans: HashSet<Span> = deep_copies
        .iter()
        .filter(|(_, t)| cx.owns_uncopyable(*t))
        .map(|(s, _)| *s)
        .collect();
    if spans.is_empty() {
        return;
    }
    visit::exprs_mut(b, &mut |e: &mut Expr| {
        if let E::Call {
            callee: c @ Callee::Intrinsic(Intrinsic::Clone),
            ..
        } = &mut e.kind
        {
            if spans.contains(&e.span) {
                *c = Callee::Intrinsic(Intrinsic::Share);
            }
        }
    });
}

/// The shared captures (`super::shares::shared_captures`) of every closure created in `b`.
fn shared_captures_in(cx: &mut Ctx, b: &mut crate::hir::Block) -> HashMap<DefId, Vec<LocalId>> {
    let mut closures = vec![];
    visit::exprs_mut(b, &mut |e: &mut Expr| {
        if let E::Closure(c) = e.kind {
            closures.push(c);
        }
    });
    closures
        .into_iter()
        .map(|c| (c, super::shares::shared_captures(cx, c)))
        .collect()
}

struct Validator<'a, 'c, 'm> {
    cx: &'c Ctx<'m>,
    f: &'a FnDef,
    kinds: &'a [LocalKind],
    /// Shared captures per closure created in the body.
    shared: &'a HashMap<DefId, Vec<LocalId>>,
    fixed: bool,
    /// See `FnInfo::keeps_fn_params`.
    keeps_fn_params: bool,
}

impl Validator<'_, '_, '_> {
    fn check(&self, e: &Expr, errors: &mut Vec<Diagnostic>) {
        match &e.kind {
            E::Local(l, UseMode::Move) => self.root(*l, None, e.span, errors),
            E::Field {
                base,
                mode: UseMode::Move,
                ..
            }
            | E::UnwrapSome(base, UseMode::Move)
            | E::UnwrapVariant {
                expr: base,
                mode: UseMode::Move,
                ..
            } => self.projection(e, base, errors),
            E::Index {
                mode: UseMode::Move,
                ..
            } => errors.push(array_move(e.span)),
            E::Call {
                callee: Callee::Intrinsic(Intrinsic::Clone | Intrinsic::Share),
                args,
            } => {
                if let [a] = args.as_slice() {
                    errors.extend(super::fn_values::borrowed_fn_copy(
                        self.cx,
                        self.f,
                        self.kinds,
                        self.keeps_fn_params,
                        a,
                    ));
                }
            }
            E::Closure(def) => {
                if let Some(Def::Fn(c)) = &self.cx.defs[def.0 as usize] {
                    let shared = self.shared.get(def).cloned().unwrap_or_default();
                    for cap in c.captures.iter().filter(|c| c.mode == PassMode::Owned) {
                        // An escaping closure keeps its by-value captures: a borrowed function
                        // parameter may be a closure living in a caller's frame (functions.md
                        // "Captures"). A local one ends with the call, like the parameter.
                        let escaping = self.cx.fn_info(*def).escaping;
                        let outer = Expr {
                            kind: E::Local(cap.outer, UseMode::Copy),
                            ty: self.f.body.locals[cap.outer.0 as usize].ty,
                            span: e.span,
                        };
                        let kept = escaping
                            .then(|| {
                                super::fn_values::borrowed_fn_copy(
                                    self.cx,
                                    self.f,
                                    self.kinds,
                                    self.keeps_fn_params,
                                    &outer,
                                )
                            })
                            .flatten();
                        if let Some(err) = kept {
                            errors.push(err);
                        } else if !shared.contains(&cap.outer) {
                            self.root(cap.outer, None, e.span, errors);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// Moving out of `e` = `base.<field>` / payload of `base`: the path must not go through an
    /// array element, a class instance or a constant, and its root must be owned.
    fn projection(&self, e: &Expr, base: &Expr, errors: &mut Vec<Diagnostic>) {
        let mut cur = base;
        loop {
            if self.cx.class_of(cur.ty).is_some() {
                let promise = matches!(self.cx.ty.kind(e.ty), crate::hir::TyKind::Promise(..));
                let note = if promise {
                    format!(
                        "{}: await the promise before storing it in the object, or keep it \
                         outside the object (in an array, taken out with `pop()`)",
                        crate::promise_copies::WHY
                    )
                } else {
                    "use `.clone()` to copy the field's value".to_string()
                };
                return errors.push(
                    Diagnostic::error("cannot move a field out of a class instance", e.span)
                        .with_note(note),
                );
            }
            match &cur.kind {
                E::Local(l, _) => {
                    let what = place_text(self.cx, &self.f.body.locals, e);
                    return self.root(*l, Some(&what), e.span, errors);
                }
                E::Index { .. } => return errors.push(array_move(e.span)),
                E::Field { base, .. }
                | E::UnwrapSome(base, _)
                | E::UnwrapVariant { expr: base, .. }
                | E::Downcast(base) => cur = base,
                E::Global(_) => {
                    return errors.push(
                        Diagnostic::error("cannot move out of a module constant", e.span)
                            .with_note("use `.clone()` for an owned copy"),
                    )
                }
                _ => return,
            }
        }
    }

    /// The shared variables closure `c` captures by value but may not move (those captures
    /// become shares).
    fn shared_captures_pinned(&self, c: DefId) -> Vec<LocalId> {
        let shared = self.shared.get(&c).cloned().unwrap_or_default();
        shared
            .into_iter()
            .filter(|&outer| {
                let mut invalid = vec![];
                self.root(outer, None, Span::default(), &mut invalid);
                !invalid.is_empty()
            })
            .collect()
    }

    /// Is the place `e` rooted at a captured variable?
    fn captured(&self, e: &Expr) -> bool {
        crate::body::places::place_root(e)
            .is_some_and(|l| matches!(self.kinds.get(l.0 as usize), Some(LocalKind::Capture)))
    }

    fn param_mode(&self, l: LocalId) -> Option<PassMode> {
        self.f.params.iter().find(|p| p.local == l).map(|p| p.mode)
    }

    /// Moving out of local `l` (or out of `what`, a place rooted at it).
    fn root(&self, l: LocalId, what: Option<&str>, span: Span, errors: &mut Vec<Diagnostic>) {
        let name = &self.f.body.locals[l.0 as usize].name;
        let (msg, note) = match self.kinds.get(l.0 as usize) {
            Some(LocalKind::Param | LocalKind::This) => {
                if matches!(self.param_mode(l), Some(PassMode::Owned | PassMode::Copy)) {
                    return;
                }
                let why = if self.fixed {
                    "parameters of closures and of overridden or interface methods are always borrowed"
                } else {
                    "it is borrowed from the caller"
                };
                let w = what.unwrap_or(name);
                let msg = match what {
                    Some(w) => format!("cannot move `{w}` out of `{name}`, which is borrowed"),
                    None => format!("cannot move out of `{name}`, which is borrowed"),
                };
                (msg, format!("{why}; use `{w}.clone()` for an owned copy"))
            }
            // A mutable element binding is a copy of a copyable element (`for (let x of [1, 2])`,
            // `body/pattern.rs`), not a borrow of it: a closure capturing it by value (one
            // assigning it) takes a copy.
            Some(LocalKind::Elem) if self.f.body.locals[l.0 as usize].mutable => return,
            Some(LocalKind::Elem) => (
                format!(
                    "cannot move out of `{}`, which borrows an array element",
                    what.unwrap_or(name)
                ),
                format!(
                    "use `{}.clone()`, or index the array and `pop()`/swap the element out",
                    what.unwrap_or(name)
                ),
            ),
            Some(LocalKind::Capture) => (
                format!("cannot move captured variable `{name}` out of the closure"),
                format!(
                    "the closure may run more than once; use `{}.clone()`",
                    what.unwrap_or(name)
                ),
            ),
            _ => return,
        };
        errors.push(Diagnostic::error(msg, span).with_note(note));
    }
}

/// Source-like text of a place (`t.name`, `p.pair.0`, `xs[..]`).
pub(super) fn place_text(cx: &Ctx, locals: &[LocalDef], e: &Expr) -> String {
    match &e.kind {
        E::Local(l, _) => locals[l.0 as usize].name.clone(),
        E::UnwrapSome(base, _) | E::UnwrapVariant { expr: base, .. } | E::Downcast(base) => {
            place_text(cx, locals, base)
        }
        E::Index { base, .. } => format!("{}[..]", place_text(cx, locals, base)),
        E::Field { base, index, .. } => {
            let field = match cx.ty.kind(base.ty) {
                crate::hir::TyKind::Adt(d, _) => cx
                    .adt(*d)
                    .and_then(|a| a.fields.get(*index as usize))
                    .map(|f| f.name.clone()),
                _ => None,
            };
            let field = field.unwrap_or_else(|| index.to_string());
            format!("{}.{field}", place_text(cx, locals, base))
        }
        _ => "the value".into(),
    }
}

fn array_move(span: Span) -> Diagnostic {
    Diagnostic::error(
        "cannot move out of an array element; use .clone() or pop()",
        span,
    )
    .with_note("elements stay owned by the array; `xs[i].clone()` makes an owned copy")
}

/// Function values borrow their arguments; the value's thunk takes another reference to the
/// ones a function owns (#224), which a value holding a promise (one owner) cannot give. A
/// generic function is checked at its type arguments; a parameter type that still mentions a
/// type parameter (a value taken inside a generic function) may hold a promise, so it is
/// rejected.
fn fn_values(cx: &mut Ctx) {
    let values = cx.fn_values.clone();
    for (d, args, span) in values {
        let params: Vec<(String, TyId)> = cx
            .fn_info(d)
            .params
            .iter()
            .filter(|p| p.mode == PassMode::Owned)
            .map(|p| (p.name.clone(), p.ty))
            .collect();
        let mut found = None;
        for (pname, t) in params {
            let t = cx.subst(t, &args);
            if cx.mentions_params(t) {
                found = Some((
                    pname,
                    "its type depends on a type parameter, which may be a promise",
                ));
                break;
            }
            if !cx.is_copy(t) && !cx.is_shared_value(t) {
                found = Some((pname, "a promise has one owner"));
                break;
            }
        }
        let Some((pname, why)) = found else {
            continue;
        };
        let fname = cx.fn_info(d).name.clone();
        cx.error(
            Diagnostic::error(
                format!("function `{fname}` takes ownership of `{pname}`, so it cannot be used as a function value"),
                span,
            )
            .with_note(format!("function values borrow their arguments, and {why}; call the function directly")),
        );
    }
}
