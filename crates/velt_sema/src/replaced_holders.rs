//! An assignment into an object literal stored inline in its parent, whose right-hand side may
//! replace that object (#876): `o.inner.v = g()` where `g` runs `o.inner = { … }`. JavaScript
//! evaluates the target first, so the old object (which nothing refers to any more) gets the
//! value and the new one keeps its own. Velt stores an object literal nothing else refers to
//! inside its holder, so the target is formed after the right-hand side and the write would
//! land in the new object. That silent difference is an error instead; the fix computes the
//! value first (`const v = g(); o.inner.v = v;`), which TypeScript runs the same way.
//!
//! Only where the difference can happen:
//! - the target's object is an object literal type (`AdtKind::Anon`, not one that contains
//!   itself, which is always a counted box) read as a field of another value;
//! - nothing in the program shares a value of that type (`Intrinsic::Share`, a capture that
//!   shares, a cell, or a shared value holding it inline): a shared object is a counted box,
//!   and lowering writes to the object the target named before the right-hand side ran;
//! - the right-hand side may assign a whole value of the type of the target's object or of a
//!   holder above it (up to the nearest class object or shared value, which lowering pins):
//!   directly, or in a function it calls (transitively). A call through a function value may
//!   run any function the program uses as a value, a call through a vtable or an interface any
//!   method, and an `await` anything. A right-hand side that only reads, computes and builds values compiles.

use std::collections::HashSet;

use velt_common::{Diagnostic, Span};

use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::hir::{AdtKind, Callee, Def, DefId, Expr, ExprKind as E, Intrinsic, TyId, TyKind};
use crate::visit;

/// One assignment whose target lies in an inline object literal.
struct Candidate {
    span: Span,
    value_span: Span,
    /// What the right-hand side runs.
    value: Effects,
    /// The type of the object the target lies in.
    holder: TyId,
    /// The types whose assignment replaces that object: it and its holders, up to a class
    /// object (pinned by lowering).
    path: Vec<TyId>,
    /// The holder as written (`o.inner`), its parent (`o`) and the target (`o.inner.v`).
    holder_text: String,
    parent_text: String,
    target_text: String,
    /// The right-hand side when it is a plain call (`g()`), for the fix.
    value_text: Option<String>,
    compound: Option<&'static str>,
}

/// What running an expression may assign directly, and what it calls.
#[derive(Default)]
struct Effects {
    /// Types of the places it assigns whole values to.
    assigns: HashSet<TyId>,
    /// It assigns a field whose type mentions a type parameter: any type, once instantiated.
    any: bool,
    /// Functions it calls directly (`new` calls the constructor).
    calls: Vec<DefId>,
    /// It calls a function value (directly or by handing it to an intrinsic): any function
    /// the program uses as a value may run.
    indirect: bool,
    /// It calls a method through a vtable, an interface or a type parameter's bound.
    methods: bool,
    /// It awaits: anything in the program may run meanwhile. (Only an `await` in the
    /// right-hand side itself counts: a function it calls runs up to its first `await` and
    /// returns, and nothing else runs until the right-hand side is done.)
    anything: bool,
    /// Functions it uses as values (closures it creates, functions it refers to).
    values: Vec<DefId>,
}

impl Effects {
    fn runs_values(&self) -> bool {
        self.indirect || self.methods || self.anything
    }

    fn note(&mut self, cx: &Ctx, e: &Expr) {
        match &e.kind {
            E::Assign { place, .. } => {
                let mut ps = vec![];
                crate::types::collect_params(&cx.ty, place.ty, &mut ps);
                let generic = !ps.is_empty() || matches!(cx.ty.kind(place.ty), TyKind::Param(_));
                // Only a field holds an object literal inline (an element replaced is #820).
                if generic && matches!(place.kind, E::Field { .. }) {
                    self.any = true;
                }
                self.assigns.insert(place.ty);
            }
            E::Call { callee, args } => match callee {
                Callee::Def(d, _) => self.calls.push(*d),
                Callee::Intrinsic(_) => {
                    // A callback handed to an intrinsic (`map`) runs.
                    self.indirect |= args.iter().any(|a| {
                        matches!(cx.ty.kind(a.ty), TyKind::FnPtr { .. } | TyKind::Closure(_))
                    });
                }
                Callee::Indirect(_) => self.indirect = true,
                _ => self.methods = true,
            },
            E::New { def, .. } => match cx.info.get(def.0 as usize) {
                Some(DefInfo::Adt(a)) => self.calls.extend(a.ctor),
                _ => self.anything = true,
            },
            E::Closure(d) | E::FnRef(d, _) => self.values.push(*d),
            E::Await(_) => self.anything = true,
            _ => {}
        }
    }
}

/// Report the assignments the module docs describe.
pub(crate) fn check(cx: &mut Ctx) {
    let mut fns: Vec<Option<Effects>> = (0..cx.defs.len()).map(|_| None).collect();
    let (mut values, mut methods) = (Vec::new(), Vec::new());
    let mut shared: Vec<TyId> = Vec::new();
    let mut candidates: Vec<Candidate> = Vec::new();
    for (i, slot) in fns.iter_mut().enumerate() {
        let Some(Def::Fn(mut f)) = cx.defs[i].take() else {
            continue;
        };
        for l in &f.body.locals {
            if l.boxed {
                shared.push(l.ty);
            }
        }
        let names: Vec<String> = f.body.locals.iter().map(|l| l.name.clone()).collect();
        let captures: Vec<_> = f
            .captures
            .iter()
            .filter(|c| c.share)
            .map(|c| c.inner)
            .collect();
        for c in captures {
            if let Some(l) = f.body.locals.get(c.0 as usize) {
                shared.push(l.ty);
            }
        }
        let mut fx = Effects::default();
        visit::exprs_mut(&mut f.body.block, &mut |e: &mut Expr| {
            fx.note(cx, e);
            match &e.kind {
                E::Call {
                    callee: Callee::Intrinsic(Intrinsic::Share),
                    ..
                } => shared.push(e.ty),
                E::Assign { place, value } => {
                    candidates.extend(candidate(cx, &names, e.span, place, value, None));
                }
                E::CompoundAssign { op, place, value } => {
                    let op = Some(op_text(*op));
                    candidates.extend(candidate(cx, &names, e.span, place, value, op));
                }
                _ => {}
            }
        });
        values.extend(fx.values.iter().copied());
        if f.self_ty.is_some() {
            methods.push(DefId(i as u32));
        }
        *slot = Some(fx);
        cx.defs[i] = Some(Def::Fn(f));
    }
    if candidates.is_empty() {
        return;
    }
    let shared = close_shared(cx, shared);
    let program = Program {
        fns,
        values,
        methods,
    };
    for c in candidates {
        if shared.contains(&c.holder) {
            continue;
        }
        // The holders lowering does not pin: up to the nearest shared value.
        let open: Vec<TyId> = c
            .path
            .iter()
            .copied()
            .take_while(|t| !shared.contains(t))
            .collect();
        if program.may_assign(&c.value, &open) {
            report(cx, &c);
        }
    }
}

/// Every function's effects, and the functions that may run through a function value or a
/// method call.
struct Program {
    fns: Vec<Option<Effects>>,
    /// Functions used as values anywhere.
    values: Vec<DefId>,
    /// Methods (functions with a `this`).
    methods: Vec<DefId>,
}

impl Program {
    /// May running the right-hand side `rhs` (its direct effects) assign a value of one of
    /// `tys`, directly or in what it calls?
    fn may_assign(&self, rhs: &Effects, tys: &[TyId]) -> bool {
        let hits = |fx: &Effects| fx.any || tys.iter().any(|t| fx.assigns.contains(t));
        if rhs.anything {
            return self.fns.iter().flatten().any(hits) || hits(rhs);
        }
        let mut seen = HashSet::new();
        let mut work = vec![rhs];
        let (mut values, mut methods) = (false, false);
        while let Some(fx) = work.pop() {
            if hits(fx) {
                return true;
            }
            let mut next: Vec<DefId> = fx.calls.clone();
            if fx.indirect && !values {
                values = true;
                next.extend(&self.values);
            }
            if fx.methods && !methods {
                methods = true;
                next.extend(&self.methods);
            }
            for d in next {
                if seen.insert(d) {
                    if let Some(Some(callee)) = self.fns.get(d.0 as usize) {
                        work.push(callee);
                    }
                }
            }
        }
        false
    }
}

fn op_text(op: crate::hir::BinOp) -> &'static str {
    use crate::hir::BinOp as B;
    match op {
        B::Add => "+=",
        B::Sub => "-=",
        B::Mul => "*=",
        B::Div => "/=",
        B::Rem => "%=",
        _ => "op=",
    }
}

fn report(cx: &mut Ctx, c: &Candidate) {
    let holder = &c.holder_text;
    let op = c.compound.unwrap_or("=");
    let value = c.value_text.as_deref().unwrap_or("…");
    let d = Diagnostic::error(
        format!(
            "the right-hand side may replace `{holder}`, the object `{target} {op} …` writes into",
            target = c.target_text
        ),
        c.span,
    )
    .with_label(c.value_span, format!("this may assign a new object to `{holder}`"))
    .with_note(format!(
        "JavaScript picks the object before the right-hand side runs and writes into the old one, which nothing refers to any more; `{holder}` is an object literal stored inside `{parent}`, so Velt would write into the new one",
        parent = c.parent_text
    ))
    .with_note(format!(
        "compute the value first: `const v = {value}; {target} {op} v;`",
        target = c.target_text
    ));
    cx.error(d);
}

/// The assignment `place op= value` as a [`Candidate`], when its target lies in an object
/// literal read as a field of another value and the right-hand side may run code.
fn candidate(
    cx: &Ctx,
    names: &[String],
    span: Span,
    place: &Expr,
    value: &Expr,
    compound: Option<&'static str>,
) -> Option<Candidate> {
    let E::Field { base, index, .. } = &place.kind else {
        return None;
    };
    let mut fx = Effects::default();
    let mut b = crate::hir::Block {
        stmts: vec![],
        value: Some(Box::new(value.clone())),
        span: value.span,
    };
    visit::exprs_mut(&mut b, &mut |x: &mut Expr| fx.note(cx, x));
    if fx.assigns.is_empty() && !fx.any && fx.calls.is_empty() && !fx.runs_values() {
        return None;
    }
    let holder = base.ty;
    let TyKind::Adt(d, _) = cx.ty.kind(holder) else {
        return None;
    };
    if adt_kind(cx, *d) != Some(AdtKind::Anon) || contains_itself(cx, *d) {
        return None;
    }
    let mut path = Vec::new();
    let h = peel(base, &mut path);
    let E::Field { base: parent, .. } = &h.kind else {
        return None;
    };
    // The holders above: their types, up to a class object (pinned by lowering).
    let mut e: &Expr = parent;
    loop {
        if is_class(cx, e.ty) {
            break;
        }
        let inner = peel(e, &mut path);
        match &inner.kind {
            E::Field { base, .. } => e = base,
            _ => break,
        }
    }
    Some(Candidate {
        span,
        value_span: value.span,
        value: fx,
        holder,
        path,
        holder_text: text(cx, names, h),
        parent_text: text(cx, names, parent),
        target_text: format!("{}.{}", text(cx, names, h), field_name(cx, holder, *index)),
        value_text: call_text(cx, names, value),
        compound,
    })
}

/// `e` without the narrowing around it (`o.inner!`), recording the type of each layer.
fn peel<'e>(mut e: &'e Expr, types: &mut Vec<TyId>) -> &'e Expr {
    loop {
        types.push(e.ty);
        match &e.kind {
            E::UnwrapSome(x, _) | E::Downcast(x) | E::UnwrapVariant { expr: x, .. } => e = x,
            _ => return e,
        }
    }
}

fn adt_kind(cx: &Ctx, d: DefId) -> Option<AdtKind> {
    match cx.info.get(d.0 as usize)? {
        DefInfo::Adt(a) => Some(a.kind),
        _ => None,
    }
}

fn is_class(cx: &Ctx, t: TyId) -> bool {
    matches!(cx.ty.kind(t), TyKind::Adt(d, _) if adt_kind(cx, *d) == Some(AdtKind::Class))
}

/// Does the object type `d` reach itself through values stored inline (always a counted box)?
fn contains_itself(cx: &Ctx, d: DefId) -> bool {
    let mut seen = HashSet::new();
    let mut work: Vec<TyId> = field_types(cx, d);
    while let Some(t) = work.pop() {
        if !seen.insert(t) {
            continue;
        }
        match cx.ty.kind(t) {
            TyKind::Adt(x, _) if *x == d => return true,
            TyKind::Adt(x, _) if adt_kind(cx, *x) != Some(AdtKind::Class) => {
                work.extend(field_types(cx, *x))
            }
            TyKind::Option(x) => work.push(*x),
            TyKind::Tuple(ts) => work.extend(ts.iter().copied()),
            _ => {}
        }
    }
    false
}

fn field_types(cx: &Ctx, d: DefId) -> Vec<TyId> {
    match cx.info.get(d.0 as usize) {
        Some(DefInfo::Adt(a)) => a.fields.iter().map(|f| f.ty).collect(),
        Some(DefInfo::Enum(e)) => e.variants.iter().flat_map(|v| v.payload.clone()).collect(),
        _ => vec![],
    }
}

/// The types shared values hold inline: a shared object literal type that is never changed in
/// place is copied field by field, sharing what its fields hold.
fn close_shared(cx: &mut Ctx, mut work: Vec<TyId>) -> HashSet<TyId> {
    let assigned = crate::assigned_fields::assigned_objects(cx);
    let mut out = HashSet::new();
    while let Some(t) = work.pop() {
        if !out.insert(t) {
            continue;
        }
        match cx.ty.kind(t).clone() {
            TyKind::Option(x) => work.push(x),
            TyKind::Tuple(ts) => work.extend(ts),
            TyKind::Adt(d, args) => {
                let kind = adt_kind(cx, d);
                if kind == Some(AdtKind::Class) || assigned.contains(&d) {
                    continue;
                }
                for f in field_types(cx, d) {
                    let f = cx.subst(f, &args);
                    work.push(f);
                }
            }
            _ => {}
        }
    }
    out
}

fn field_name(cx: &Ctx, t: TyId, index: u32) -> String {
    match cx.ty.kind(t) {
        TyKind::Adt(d, _) => match cx.info.get(d.0 as usize) {
            Some(DefInfo::Adt(a)) => a
                .fields
                .get(index as usize)
                .map(|f| f.name.clone())
                .unwrap_or_default(),
            _ => String::new(),
        },
        _ => String::new(),
    }
}

/// A place as written (`o.inner`), for the message.
fn text(cx: &Ctx, names: &[String], e: &Expr) -> String {
    match &e.kind {
        E::Local(l, _) => names.get(l.0 as usize).cloned().unwrap_or_default(),
        E::Field { base, index, .. } => {
            format!(
                "{}.{}",
                text(cx, names, base),
                field_name(cx, base.ty, *index)
            )
        }
        E::UnwrapSome(x, _) | E::Downcast(x) | E::UnwrapVariant { expr: x, .. } => {
            text(cx, names, x)
        }
        E::Index { base, .. } => format!("{}[…]", text(cx, names, base)),
        _ => "…".into(),
    }
}

/// `g()` for a call of a named function or variable without arguments.
fn call_text(cx: &Ctx, names: &[String], e: &Expr) -> Option<String> {
    let E::Call { callee, args } = &e.kind else {
        return None;
    };
    let name = match callee {
        Callee::Indirect(f) => match &f.kind {
            E::Local(..) => text(cx, names, f),
            _ => return None,
        },
        Callee::Def(d, _) => match cx.info.get(d.0 as usize) {
            Some(DefInfo::Fn(f)) if !f.name.contains("::") && !f.name.contains('.') => {
                f.name.clone()
            }
            _ => return None,
        },
        _ => return None,
    };
    let args = if args.is_empty() { "" } else { "…" };
    Some(format!("{name}({args})"))
}
