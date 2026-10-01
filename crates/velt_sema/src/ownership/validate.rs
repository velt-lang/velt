//! After ownership inference: every move must come out of something owned. Reports moves out
//! of array elements, class-instance fields, borrowed parameters / `this`, `for...of` elements
//! and captured variables (each with a `.clone()` hint), and functions used as values whose
//! parameters are owned (a function value's ABI borrows).

use std::collections::HashSet;

use velt_common::{Diagnostic, Span};

use crate::body::LocalKind;
use crate::ctx::Ctx;
use crate::defs::BodyState;
use crate::hir::{
    Callee, Def, DefId, Expr, ExprKind as E, FnDef, Intrinsic, LocalDef, LocalId, PassMode, UseMode,
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
        let kinds = cx.fn_info(d).local_kinds.clone();
        let fixed = cx.fn_info(d).fixed_modes;
        let soft: HashSet<Span> = cx.fn_info(d).soft_moves.iter().copied().collect();
        let v = Validator {
            cx,
            f: &f,
            kinds: &kinds,
            fixed,
        };
        let mut block = f.body.block.clone();
        visit::exprs_mut(&mut block, &mut |e: &mut Expr| {
            if soft.contains(&e.span) && soft::is_moved_place(e) {
                let mut invalid = vec![];
                v.check(e, &mut invalid);
                if !invalid.is_empty() {
                    match super::fn_values::borrowed_fn_copy(v.cx, v.f, v.kinds, e) {
                        Some(err) => errors.push(err),
                        None => soft::make_clone(e),
                    }
                    return;
                }
            }
            if let (E::Closure(c), true) = (&e.kind, soft.contains(&e.span)) {
                if v.string_captures_pinned(*c) {
                    copied_captures.push(*c);
                }
            }
            v.check(e, &mut errors)
        });
        cx.diags.extend(errors);
        f.body.block = block;
        cx.defs[d.0 as usize] = Some(Def::Fn(f));
        for c in copied_captures {
            super::strings::copy_string_captures(cx, c);
        }
    }
    fn_values(cx);
}

struct Validator<'a, 'c, 'm> {
    cx: &'c Ctx<'m>,
    f: &'a FnDef,
    kinds: &'a [LocalKind],
    fixed: bool,
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
                callee: Callee::Intrinsic(Intrinsic::Clone),
                args,
            } => {
                if let [a] = args.as_slice() {
                    errors.extend(super::fn_values::borrowed_fn_copy(
                        self.cx, self.f, self.kinds, a,
                    ));
                }
            }
            E::Closure(def) => {
                if let Some(Def::Fn(c)) = &self.cx.defs[def.0 as usize] {
                    let strings: Vec<LocalId> = super::strings::string_captures(self.cx, *def)
                        .map(|cap| cap.outer)
                        .collect();
                    for cap in c.captures.iter().filter(|c| c.mode == PassMode::Owned) {
                        if !strings.contains(&cap.outer) {
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
                return errors.push(
                    Diagnostic::error("cannot move a field out of a class instance", e.span)
                        .with_note("use `.clone()` to copy the field's value"),
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
                | E::UnwrapVariant { expr: base, .. } => cur = base,
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

    /// Does closure `c` capture by value a string variable it may not move (then its string
    /// captures become copies)?
    fn string_captures_pinned(&self, c: DefId) -> bool {
        super::strings::string_captures(self.cx, c).any(|cap| {
            let mut invalid = vec![];
            self.root(cap.outer, None, Span::default(), &mut invalid);
            !invalid.is_empty()
        })
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
        E::UnwrapSome(base, _) | E::UnwrapVariant { expr: base, .. } => {
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

/// Function values borrow their arguments: a function with owned params can't be one.
fn fn_values(cx: &mut Ctx) {
    let values = cx.fn_values.clone();
    for (d, span) in values {
        let f = cx.fn_info(d);
        if let Some(p) = f.params.iter().find(|p| p.mode == PassMode::Owned) {
            let (fname, pname) = (f.name.clone(), p.name.clone());
            cx.error(
                Diagnostic::error(
                    format!("function `{fname}` takes ownership of `{pname}`, so it cannot be used as a function value"),
                    span,
                )
                .with_note("function values borrow their arguments; wrap it in a closure that clones"),
            );
        }
    }
}
