//! A generic interface value where the same interface with `unknown` (or `any`) type arguments
//! is expected (#1153; docs/reference/types.md "Generic interfaces with `unknown` arguments"):
//! `Handler<Date, number>` as a `Handler` (`Handler<unknown, unknown>`). TypeScript accepts it,
//! since it checks method parameters bivariantly; Velt checks the arguments at run time instead.
//!
//! The value is wrapped in an adapter, a struct no source names whose one field holds the
//! original interface value, with an implementation of `I<unknown, …>` (`Program::impls`). Each
//! slot calls the same slot of the wrapped value:
//!
//! - each argument converts from its `unknown` type to the concrete one, checked at run time like
//!   an `any` (`Intrinsic::AnyTo`: a `TypeError` on a mismatch, which TypeScript and Node don't
//!   check);
//! - the result is boxed (a covariant position needs no check), and so is a field read through
//!   its getter slot;
//! - a function value in either position (an optional method `revive?(e: E): T` is a field of
//!   function type) is wrapped in an arrow that converts its own arguments and result the same
//!   way, and an interface value in a result by its own adapter;
//! - an argument for a parameter of type `unknown[]`, `Map<unknown, unknown>` or `Set<unknown>`
//!   ([`Container`]), which may hold elements of any type, is passed as a copy holding them
//!   boxed, written back into the caller's container when the call returns (the prelude
//!   helpers of std/prelude/zzz_adapt.vlt);
//! - another parameter type holding `unknown` (`Box<unknown>`) refuses the conversion, with the
//!   reason in the mismatch's note ([`FnCx::adapt_refusal`]).
//!
//! The adapter is a counted object (`Program::counted_objects`): a copy of the converted value
//! is another reference to the same adapter, so every copy reaches the one original.
//!
//! One adapter exists per interface and pair of argument lists that a conversion in the program
//! uses (`Ctx::iface_adapters`), made when the first such conversion is checked: programs without
//! one are unchanged.
//!
//! ```text
//! const all: Handler[] = [dateHandler];
//!   →  [<Handler adapter> { inner: dateHandler } as Handler<unknown, unknown>]
//! ```

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::callback::synth;
use super::callback_wrap::{wrapper, Shape};
use super::iface_adapt_slots::{adapter_method, AdapterParts};
use crate::any::Dynamic;
use crate::body::{FnCx, LocalKind};
use crate::ctx::Ctx;
use crate::defs::ParamSig;
use crate::hir::{self, Def, DefId, ExprKind as H, ImplDef, TyId, TyKind};
use crate::iface_writes::SetterSrc;

/// The interface, the value's type arguments and the expected ones.
pub(crate) type AdapterKey = (DefId, Vec<TyId>, Vec<TyId>);

/// An adapter (`Ctx::iface_adapters`): its struct and its implementation of the interface.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Adapter {
    pub def: DefId,
    pub impl_index: u32,
}

/// One slot of the interface: its name, parameters and result, in terms of the interface's
/// type parameters.
pub(super) struct SlotSig {
    pub name: String,
    pub params: Vec<ParamSig>,
    pub ret: TyId,
}

/// The slots of `iface` in table order (methods, then the getters of its fields), or `None`
/// when a generic method has no slot to adapt.
fn slot_sigs(cx: &Ctx, iface: DefId) -> Option<Vec<SlotSig>> {
    let info = cx.iface(iface)?;
    let mut out = vec![];
    for m in &info.methods {
        if !m.generics.names.is_empty() {
            return None;
        }
        out.push(SlotSig {
            name: m.name.clone(),
            params: m.params.clone(),
            ret: m.ret,
        });
    }
    for f in &info.fields {
        out.push(SlotSig {
            name: format!("<{}>", f.name),
            params: vec![],
            ret: f.ty,
        });
    }
    Some(out)
}

/// Does `t` mention a type parameter or an unknown type?
fn open(cx: &Ctx, t: TyId) -> bool {
    let mut found = cx.ty.has_error(t);
    cx.ty
        .visit(t, &mut |k| found |= matches!(k, TyKind::Param(_)));
    found
}

/// A container parameter type the adapter passes as a copy of boxed elements, written back
/// after the call (std/prelude/zzz_adapt.vlt): `unknown[]`, `Map<unknown, unknown>` and
/// `Set<unknown>`, the shapes of a serializer's handler table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Container {
    Array,
    Map,
    Set,
}

impl Container {
    /// The prelude helpers making the copy and writing it back.
    pub(super) fn helpers(self) -> (&'static str, &'static str) {
        match self {
            Container::Array => ("__unknownArrayIn", "__unknownArrayOut"),
            Container::Map => ("__unknownMapIn", "__unknownMapOut"),
            Container::Set => ("__unknownSetIn", "__unknownSetOut"),
        }
    }
}

/// Is `t` one of the [`Container`] types?
pub(super) fn container(cx: &Ctx, t: TyId) -> Option<Container> {
    let unknown = |x: TyId| cx.dynamic(x) == Some(Dynamic::Unknown);
    match cx.ty.kind(t) {
        TyKind::Array(e) if unknown(*e) => Some(Container::Array),
        TyKind::Adt(d, args) => {
            let a = cx.adt(*d)?;
            match (a.qual_name.as_str(), args.as_slice()) {
                ("std/prelude/map::Map", [k, v]) if unknown(*k) && unknown(*v) => {
                    Some(Container::Map)
                }
                ("std/collections/set::Set", [e]) if unknown(*e) => Some(Container::Set),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Does `t` hold `unknown` or `any` somewhere inside (not as itself)?
fn holds_dynamic(cx: &Ctx, t: TyId) -> bool {
    let Some(defs) = cx.any else {
        return false;
    };
    let mut found = false;
    cx.ty.visit(
        t,
        &mut |k| found |= matches!(k, TyKind::Dyn(d, _) if *d == defs.any || *d == defs.unknown),
    );
    found
}

/// Does `t` hold a generic class (other than `Map` and `Set`) with an `unknown` or `any`
/// type argument (`Box<unknown>`)?
fn dynamic_class(cx: &Ctx, t: TyId) -> bool {
    let mut found = false;
    cx.ty.visit(t, &mut |k| {
        if let TyKind::Adt(d, args) = k {
            let class = cx.adt(*d).is_some_and(|a| {
                a.kind == hir::AdtKind::Class
                    && a.qual_name != "std/prelude/map::Map"
                    && a.qual_name != "std/collections/set::Set"
            });
            found |= class
                && args
                    .iter()
                    .any(|a| cx.dynamic(*a).is_some() || holds_dynamic(cx, *a));
        }
    });
    found
}

/// Can a value of `iface<from>` be adapted to `iface<to>`: each expected argument is the
/// value's own or dynamic, and every slot's parameters and result convert ([`value_ok`]).
/// `seen` holds the pairs being decided (an interface whose methods return it); `why` gets
/// the reason a supported shape is refused, for the diagnostic.
fn adaptable(
    cx: &mut Ctx,
    iface: DefId,
    from: &[TyId],
    to: &[TyId],
    seen: &mut Vec<AdapterKey>,
    why: &mut Option<String>,
) -> bool {
    if from.len() != to.len()
        || from == to
        || cx.any.is_some_and(|a| a.any == iface || a.unknown == iface)
        || from.iter().any(|t| open(cx, *t))
        || !from
            .iter()
            .zip(to)
            .all(|(a, b)| a == b || matches!(cx.dynamic(*b), Some(Dynamic::Any | Dynamic::Unknown)))
    {
        return false;
    }
    let key = (iface, from.to_vec(), to.to_vec());
    if seen.contains(&key) {
        return true;
    }
    seen.push(key);
    let Some(sigs) = slot_sigs(cx, iface) else {
        *why = Some("an interface with a generic method is not supported yet (#1185)".into());
        return false;
    };
    // The implementor's type parameter stands for an unknown type: a slot mentioning it is
    // not adapted.
    let unknown = cx.ty.error;
    let (mut fa, mut ta) = (from.to_vec(), to.to_vec());
    fa.push(unknown);
    ta.push(unknown);
    let own = from.len() as u32;
    sigs.iter().all(|s| {
        if unresolved_slot(cx, s, own, &fa, &ta) {
            return true;
        }
        let (rf, rt) = (cx.subst(s.ret, &fa), cx.subst(s.ret, &ta));
        value_ok(cx, rf, rt, false, seen, why)
            && s.params.iter().all(|p| {
                let (pf, pt) = (cx.subst(p.ty, &fa), cx.subst(p.ty, &ta));
                value_ok(cx, pt, pf, true, seen, why)
            })
    })
}

/// Is slot `s` one whose type could not be resolved, in the interface (a generic function type)
/// or at these type arguments (`keyof any` in a mapped type)? Its error was reported where the
/// type is written: refusing the conversion too would add an error at every conversion site
/// for the same cause. A slot mentioning the implementor's own type parameter (`own`) is
/// decided as usual.
fn unresolved_slot(cx: &mut Ctx, s: &SlotSig, own: u32, fa: &[TyId], ta: &[TyId]) -> bool {
    let tys: Vec<TyId> = s
        .params
        .iter()
        .map(|p| p.ty)
        .chain(std::iter::once(s.ret))
        .collect();
    let mut mentions_own = false;
    for &t in &tys {
        cx.ty.visit(t, &mut |k| {
            mentions_own |= matches!(k, TyKind::Param(p) if *p == own)
        });
    }
    if mentions_own {
        return false;
    }
    tys.iter().any(|&t| {
        let (f, g) = (cx.subst(t, fa), cx.subst(t, ta));
        cx.ty.has_error(f) || cx.ty.has_error(g)
    })
}

/// Does a value of type `from` convert to `to` inside an adapter? `param` when it is a
/// parameter of the slot itself, where a [`Container`] is passed as a copy.
pub(super) fn value_ok(
    cx: &mut Ctx,
    from: TyId,
    to: TyId,
    param: bool,
    seen: &mut Vec<AdapterKey>,
    why: &mut Option<String>,
) -> bool {
    if from == to {
        return !cx.ty.has_error(from);
    }
    if open(cx, from) || open(cx, to) {
        return false;
    }
    if cx.dynamic(to).is_some() {
        return !matches!(
            cx.ty.kind(from),
            TyKind::Promise(..) | TyKind::Unit | TyKind::Never
        );
    }
    if cx.dynamic(from).is_some() {
        let t = cx.display(to);
        let reason = match cx.ty.kind(to) {
            TyKind::Promise(..) | TyKind::Unit | TyKind::Never => return false,
            // `any` converted to an interface type is not supported yet (#924).
            TyKind::Dyn(..) => Some(format!(
                "a parameter of type `{t}`: converting `unknown` to an interface is not supported yet (#924)"
            )),
            TyKind::FnPtr { .. } => Some(format!(
                "a parameter of type `{t}`: converting `unknown` to a function type is not supported yet (#1185)"
            )),
            _ if param && container(cx, to).is_some() => None,
            // Elsewhere (a parameter of a function value, such as an optional method
            // `revive?(e: E): T`) a type holding `unknown` is checked as it is, except a generic
            // class of `unknown` arguments, which a value of another instantiation never
            // matches.
            _ if !param && !dynamic_class(cx, to) => None,
            _ if holds_dynamic(cx, to) => Some(format!(
                "a parameter of type `{t}`, which holds `unknown` or `any`, is not supported yet (#1185); `unknown[]`, `Map<unknown, unknown>` and `Set<unknown>` are"
            )),
            _ => None,
        };
        if let Some(r) = reason {
            *why = Some(r);
            return false;
        }
        return true;
    }
    if let (Some(a), Some(b)) = (cx.ty.opt_payload(from), cx.ty.opt_payload(to)) {
        return cx.ty.nothing_of(from) == cx.ty.nothing_of(to)
            && value_ok(cx, a, b, false, seen, why);
    }
    match (cx.ty.kind(from).clone(), cx.ty.kind(to).clone()) {
        (
            TyKind::FnPtr {
                params: p,
                ret: r,
                throws: t,
                rest: rest_a,
            },
            TyKind::FnPtr {
                params: q,
                ret: s,
                throws: u,
                rest: rest_b,
            },
        ) => {
            p.len() == q.len()
                && rest_a == rest_b
                && t == u
                && q.iter()
                    .zip(&p)
                    .all(|(x, y)| value_ok(cx, *x, *y, false, seen, why))
                && value_ok(cx, r, s, false, seen, why)
        }
        (TyKind::Dyn(i, xs), TyKind::Dyn(j, ys)) if i == j => adaptable(cx, i, &xs, &ys, seen, why),
        _ => false,
    }
}

/// Is adapter `a` usable? A check rolled back after making it (`recheck::Mark::rollback`)
/// empties the bodies of the functions it made, and the adapter is made again.
fn intact(cx: &Ctx, a: Adapter) -> bool {
    match cx.impls[a.impl_index as usize].methods.first() {
        // Still being made (a slot's result needs the adapter itself).
        None => true,
        Some(m) => {
            matches!(&cx.defs[m.0 as usize], Some(Def::Fn(f)) if !f.body.block.stmts.is_empty())
        }
    }
}

impl FnCx<'_, '_> {
    /// `h`, a value of interface type `iface<from>`, as `iface<to>` (type `exp`) through an
    /// adapter (see the module docs); `Err` when it does not adapt.
    pub(super) fn adapt_iface(
        &mut self,
        mut h: hir::Expr,
        exp: TyId,
        iface: DefId,
        from: &[TyId],
        to: &[TyId],
    ) -> Result<hir::Expr, hir::Expr> {
        if !adaptable(self.cx, iface, from, to, &mut vec![], &mut None) {
            return Err(h);
        }
        let span = h.span;
        let a = self.iface_adapter(iface, from, to, span);
        self.force_move(&mut h);
        let ty = self.cx.ty.intern(TyKind::Adt(a.def, vec![]));
        let lit = self.mk(
            H::AdtLit {
                def: a.def,
                type_args: vec![],
                fields: vec![h],
                converted: hir::Conversion::No,
            },
            ty,
            span,
        );
        Ok(self.mk(
            H::ToDyn {
                expr: Box::new(lit),
                impl_index: a.impl_index,
            },
            exp,
            span,
        ))
    }

    /// Can a value implementing `iface<from>` be adapted to `iface<to>`?
    pub(super) fn iface_adapts(&mut self, iface: DefId, from: &[TyId], to: &[TyId]) -> bool {
        adaptable(self.cx, iface, from, to, &mut vec![], &mut None)
    }

    /// Why a value of type `found` does not convert to the interface type `expected` through
    /// an adapter, when the shape is one an adapter takes but a slot is refused
    /// (`report_mismatch`'s note).
    pub(super) fn adapt_refusal(&mut self, expected: TyId, found: TyId) -> Option<String> {
        let TyKind::Dyn(iface, to) = self.cx.ty.kind(expected).clone() else {
            return None;
        };
        let from = match self.cx.ty.kind(found).clone() {
            TyKind::Dyn(i, from) if i == iface => from,
            _ => self.cx.find_impl(found, iface)?.1,
        };
        let mut why = None;
        if adaptable(self.cx, iface, &from, &to, &mut vec![], &mut why) {
            return None;
        }
        why
    }

    /// The adapter from `iface<from>` to `iface<to>`, made on first use.
    fn iface_adapter(&mut self, iface: DefId, from: &[TyId], to: &[TyId], span: Span) -> Adapter {
        let key = (iface, from.to_vec(), to.to_vec());
        if let Some(&a) = self.cx.iface_adapters.get(&key) {
            if intact(self.cx, a) {
                return a;
            }
        }
        let inner = self.cx.ty.intern(TyKind::Dyn(iface, from.to_vec()));
        let iname = self
            .cx
            .iface(iface)
            .map_or(String::new(), |i| i.name.clone());
        let d = self.cx.hidden_struct_def(
            &format!("{iname} adapter"),
            &[("inner".to_string(), inner)],
            0,
            self.module,
        );
        let self_ty = self.cx.ty.intern(TyKind::Adt(d, vec![]));
        // A counted box: a share of the converted value is another reference to the same
        // adapter, so every copy reaches the one wrapped value (a copy of the adapter would
        // clone it, and a class instance with it).
        if !self.cx.counted_objects.contains(&self_ty) {
            self.cx.counted_objects.push(self_ty);
        }
        self.cx.impls.push(ImplDef {
            ty: self_ty,
            generics: 0,
            iface,
            iface_args: to.to_vec(),
            methods: vec![],
        });
        let a = Adapter {
            def: d,
            impl_index: self.cx.impls.len() as u32 - 1,
        };
        self.cx.iface_adapters.insert(key, a);
        let sigs = slot_sigs(self.cx, iface).unwrap_or_default();
        let parts = AdapterParts {
            def: d,
            self_ty,
            inner,
            iface,
            iname,
            from: from.to_vec(),
            to: to.to_vec(),
            module: self.module,
            span,
        };
        let methods = sigs
            .iter()
            .enumerate()
            .map(|(slot, s)| adapter_method(self.cx, &parts, slot as u32, s, false))
            .collect();
        self.cx.impls[a.impl_index as usize].methods = methods;
        // Its setters, made once a field of the interface is written through.
        let fields = self.cx.setter_fields(iface);
        let srcs = fields
            .into_iter()
            .map(|field| SetterSrc::Adapter {
                parts: Box::new(parts.clone()),
                field,
            })
            .collect();
        self.cx.note_setters(a.impl_index, srcs);
        // The dispatch groups gain the new methods (`throws/groups.rs`): built again on next use.
        self.cx.groups = None;
        a
    }

    /// `h` converted to `to` inside an adapter's method (`adapting`): unchanged, checked out of
    /// `unknown`, boxed, or wrapped.
    pub(super) fn adapt_value(&mut self, h: hir::Expr, to: TyId) -> hir::Expr {
        if h.ty == to {
            return h;
        }
        match self.try_coerce(h, to) {
            Ok(h) => h,
            Err(h) => {
                // `adaptable` checked every position; reported in case one still fails.
                let (f, t) = (self.cx.display(h.ty), self.cx.display(to));
                self.cx.error(Diagnostic::error(
                    format!(
                        "internal limit: cannot convert `{f}` to `{t}` in an interface adapter"
                    ),
                    h.span,
                ));
                self.error_expr(h.span)
            }
        }
    }

    /// A function value `h` as function type `exp` inside an adapter: `{ const f = h; (p…) =>
    /// f(p…) }`, the arrow checked against `exp`, which converts each argument and the result.
    pub(super) fn adapt_fn(&mut self, h: hir::Expr, exp: TyId) -> Result<hir::Expr, hir::Expr> {
        let TyKind::FnPtr { params, rest, .. } = self.cx.ty.kind(exp) else {
            return Err(h);
        };
        let shape = Shape::of(params.len(), *rest);
        let span = h.span;
        let name = ast::Ident {
            name: format!("#adapted{}", self.f.locals.len()),
            span,
        };
        self.push_scope();
        if let (H::Local(l, _), true) = (&h.kind, self.adapting) {
            let l = *l;
            if self.local_kind(l) == LocalKind::Param {
                // A function passed to the method is borrowed: the arrow calling it is passed on
                // at once and never kept, so it borrows the parameter by name.
                if let Some(scope) = self.f.scopes.last_mut() {
                    scope.names.insert(name.name.clone(), l);
                }
                let arrow = wrapper(
                    synth(ast::ExprKind::Ident(name), span),
                    shape,
                    &shape.same(),
                    false,
                    span,
                );
                let w = self.closure(&arrow, Some(exp), false);
                self.pop_scope();
                return Ok(w);
            }
        }
        let local = self.declare_local(&name, h.ty, LocalKind::Const);
        let arrow = wrapper(
            synth(ast::ExprKind::Ident(name), span),
            shape,
            &shape.same(),
            false,
            span,
        );
        let w = self.closure(&arrow, Some(exp), true);
        self.pop_scope();
        let mut h = h;
        self.force_move(&mut h);
        let ty = w.ty;
        let block = hir::Block {
            stmts: vec![hir::Stmt {
                kind: hir::StmtKind::Let {
                    local,
                    init: Some(h),
                },
                span,
            }],
            value: Some(Box::new(w)),
            span,
        };
        Ok(self.mk(H::Block(block), ty, span))
    }
}
