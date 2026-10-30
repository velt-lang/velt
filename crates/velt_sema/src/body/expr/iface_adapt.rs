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
//!   helpers of std/prelude/adapt.vlt);
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
use super::callback_wrap::{wrapper, Arg, Plan, Shape};
use super::iface_adapt_slots::{adapter_method, fields_ok, unresolved_slot, AdapterParts};
use crate::any::Dynamic;
use crate::body::{FnCx, LocalKind, Want};
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
    /// The method's own type parameters: a generic method has no table entry and is never
    /// called through an interface value, so its slot needs no adapting (`adapter_stub`).
    pub own: u32,
}

/// The slots of `iface` in table order (methods, then the getters of its fields).
fn slot_sigs(cx: &Ctx, iface: DefId) -> Option<Vec<SlotSig>> {
    let info = cx.iface(iface)?;
    let mut out = vec![];
    for m in &info.methods {
        out.push(SlotSig {
            name: m.name.clone(),
            params: m.params.clone(),
            ret: m.ret,
            own: m.generics.names.len() as u32,
        });
    }
    for f in &info.fields {
        out.push(SlotSig {
            name: format!("<{}>", f.name),
            params: vec![],
            ret: f.ty,
            own: 0,
        });
    }
    Some(out)
}

/// Does `t` mention a type parameter or an unknown type? In a `generic` adapter (made for
/// every instance of its source arguments, `iface_adapt_dynamic`) only an unknown type counts:
/// the type parameters are the adapter's own.
fn open(cx: &Ctx, t: TyId, generic: bool) -> bool {
    cx.ty.has_error(t) || (!generic && has_params(cx, t))
}

/// Does `t` mention a type parameter?
pub(super) fn has_params(cx: &Ctx, t: TyId) -> bool {
    let mut found = false;
    cx.ty
        .visit(t, &mut |k| found |= matches!(k, TyKind::Param(_)));
    found
}

/// A container parameter type the adapter passes as a copy of boxed elements, written back
/// after the call (std/prelude/adapt.vlt): `unknown[]`, `Map<unknown, unknown>` and
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
                (velt_common::std_item!(prelude_map, "Map"), [k, v])
                    if unknown(*k) && unknown(*v) =>
                {
                    Some(Container::Map)
                }
                (velt_common::std_item!(collections_set, "Set"), [e]) if unknown(*e) => {
                    Some(Container::Set)
                }
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
                    && a.qual_name != velt_common::std_item!(prelude_map, "Map")
                    && a.qual_name != velt_common::std_item!(collections_set, "Set")
            });
            found |= class
                && args
                    .iter()
                    .any(|a| cx.dynamic(*a).is_some() || holds_dynamic(cx, *a));
        }
    });
    found
}

/// How [`adaptable`] decides: the pairs being decided (an interface whose methods return it),
/// the reason a supported shape is refused (for the diagnostic), whether the source
/// arguments are a generic adapter's type parameters (`iface_adapt_dynamic`), and whether the
/// position is a parameter or result of a function type (`in_fn`).
#[derive(Default)]
pub(super) struct Mode {
    pub seen: Vec<AdapterKey>,
    pub why: Option<String>,
    pub generic: bool,
    pub in_fn: bool,
}

impl Mode {
    /// Deciding a generic adapter (`iface_adapt_dynamic`).
    pub(super) fn generic() -> Mode {
        Mode {
            generic: true,
            ..Mode::default()
        }
    }
}

/// Is `t` an expected argument that takes any value (`any`, `unknown`)?
pub(super) fn is_dynamic_arg(cx: &Ctx, t: TyId) -> bool {
    matches!(cx.dynamic(t), Some(Dynamic::Any | Dynamic::Unknown))
}

/// Can a value of `iface<from>` be adapted to `iface<to>`: each expected argument is the
/// value's own or dynamic, and every slot's parameters and result convert ([`value_ok`]).
/// In a generic adapter, an argument for a dynamic one is a type parameter of the adapter
/// (an instance of the adapter to `to` stands for it) or names none.
pub(super) fn adaptable(
    cx: &mut Ctx,
    iface: DefId,
    from: &[TyId],
    to: &[TyId],
    m: &mut Mode,
) -> bool {
    let generic = m.generic;
    let arg_ok = |cx: &Ctx, a: TyId, b: TyId| match is_dynamic_arg(cx, b) {
        true => !generic || matches!(cx.ty.kind(a), TyKind::Param(_)) || !has_params(cx, a),
        false => a == b,
    };
    if from.len() != to.len()
        || from == to
        || cx.any.is_some_and(|a| a.any == iface || a.unknown == iface)
        || from.iter().any(|t| open(cx, *t, generic))
    {
        return false;
    }
    if let Some((_, b)) = from.iter().zip(to).find(|(a, b)| !arg_ok(cx, **a, **b)) {
        // A type argument that holds `unknown` or `any` without being one (`Box<unknown>`,
        // `Map<any, any>`, `Map<string, unknown>`): only whole `unknown` / `any` arguments
        // and the containers of `Container` are adapted.
        if holds_dynamic(cx, *b) {
            let t = cx.display(*b);
            m.why = Some(format!(
                "the type argument `{t}`, which holds `unknown` or `any`: converting a generic class or container to other type arguments is not supported yet (#1185); `unknown` / `any` themselves are"
            ));
        }
        return false;
    }
    let key = (iface, from.to_vec(), to.to_vec());
    if m.seen.contains(&key) {
        return true;
    }
    m.seen.push(key);
    let Some(sigs) = slot_sigs(cx, iface) else {
        m.why = Some("an interface with a generic method is not supported yet (#1185)".into());
        return false;
    };
    // The implementor's type parameter stands for an unknown type: a slot mentioning it is
    // not adapted.
    let unknown = cx.ty.error;
    let (mut fa, mut ta) = (from.to_vec(), to.to_vec());
    fa.push(unknown);
    ta.push(unknown);
    let own = from.len() as u32;
    // The slots of this interface are positions of its own, not of a function type around it.
    let in_fn = std::mem::replace(&mut m.in_fn, false);
    let ok = sigs.iter().all(|s| {
        if s.own > 0 || unresolved_slot(cx, s, own, &fa, &ta) {
            return true;
        }
        let (rf, rt) = (cx.subst(s.ret, &fa), cx.subst(s.ret, &ta));
        value_ok(cx, rf, rt, false, m)
            && s.params.iter().all(|p| {
                let (pf, pt) = (cx.subst(p.ty, &fa), cx.subst(p.ty, &ta));
                value_ok(cx, pt, pf, true, m)
            })
    });
    m.in_fn = in_fn;
    ok
}

/// Does a value of type `from` convert to `to` inside an adapter? `param` when it is a
/// parameter of the slot itself, where a [`Container`] is passed as a copy.
pub(super) fn value_ok(cx: &mut Ctx, from: TyId, to: TyId, param: bool, m: &mut Mode) -> bool {
    if from == to {
        return !cx.ty.has_error(from);
    }
    if open(cx, from, m.generic) || open(cx, to, m.generic) {
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
            // A `never` parameter (`serialize(v: T)` at `T = never`) takes no value: a call
            // reaching it through the adapter throws, as converting any value to `never` does.
            TyKind::Never if param => None,
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
            m.why = Some(r);
            return false;
        }
        return true;
    }
    if let (Some(a), Some(b)) = (cx.ty.opt_payload(from), cx.ty.opt_payload(to)) {
        return cx.ty.nothing_of(from) == cx.ty.nothing_of(to) && value_ok(cx, a, b, false, m);
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
            let in_fn = std::mem::replace(&mut m.in_fn, true);
            let ok = p.len() == q.len()
                && rest_a == rest_b
                && t == u
                && q.iter()
                    .zip(&p)
                    .all(|(x, y)| value_ok(cx, *x, *y, false, m))
                && value_ok(cx, r, s, false, m);
            m.in_fn = in_fn;
            ok
        }
        (TyKind::Dyn(i, xs), TyKind::Dyn(j, ys)) if i == j => adaptable(cx, i, &xs, &ys, m),
        // A result that is an object type, or a union of them, whose fields convert
        // (`IteratorResult<[any, any]>` as `IteratorResult<any>`): a copy (`object_copy`).
        _ if !param && cx.union_members(from).is_some() => {
            let (Some(xs), Some(ys)) = (cx.union_members(from), cx.union_members(to)) else {
                return false;
            };
            xs.len() == ys.len()
                && xs.iter().zip(&ys).all(|(a, b)| {
                    a == b || value_ok(cx, *a, *b, false, m) || fields_ok(cx, *a, *b, m)
                })
        }
        _ if !param && fields_ok(cx, from, to, m) => true,
        // An array of interface values read through the adapter (a field `kids: Tree<T>[]`): a
        // new array of the elements, each adapted (`adapt_array`). Nothing is written back, so
        // where a value goes the other way (a parameter, a setter) it is refused, and so is one
        // passed to or returned by a function value (`withKids(f: (k: Tree<T>[]) => void)`,
        // `kidsFn: () => Tree<T>[]`), whose uses of the new array are not checked
        // (`iface_field`).
        (TyKind::Array(a), TyKind::Array(b)) if !param => {
            let same = match (cx.ty.kind(a), cx.ty.kind(b)) {
                (TyKind::Dyn(i, _), TyKind::Dyn(j, _)) => i == j,
                _ => false,
            };
            if same && m.in_fn {
                let (f, t) = (cx.display(from), cx.display(to));
                m.why = Some(format!(
                    "a function type taking or giving `{f}` as `{t}`: converting an array of interface values there is not supported yet (#1185)"
                ));
                return false;
            }
            same && value_ok(cx, a, b, false, m)
        }
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
        let span = h.span;
        // Inside a generic adapter, a value over its type parameters: an instance of the
        // generic adapter to `to` (`iface_adapt_dynamic`).
        let (a, type_args) = match from.iter().any(|t| has_params(self.cx, *t)) {
            true => match self.dynamic_adapter_at(iface, from, to, span) {
                Some(found) => found,
                None => return Err(h),
            },
            false => {
                if !adaptable(self.cx, iface, from, to, &mut Mode::default()) {
                    return Err(h);
                }
                (self.iface_adapter(iface, from, to, 0, span), vec![])
            }
        };
        self.force_move(&mut h);
        let ty = self.cx.ty.intern(TyKind::Adt(a.def, type_args.clone()));
        let lit = self.mk(
            H::AdtLit {
                def: a.def,
                type_args,
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
        adaptable(self.cx, iface, from, to, &mut Mode::default())
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
        let mut m = Mode::default();
        if adaptable(self.cx, iface, &from, &to, &mut m) {
            return None;
        }
        m.why
    }

    /// The adapter from `iface<from>` to `iface<to>`, made on first use; with `generics`, `from`
    /// is over that many type parameters, the adapter's own (`iface_adapt_dynamic`).
    pub(super) fn iface_adapter(
        &mut self,
        iface: DefId,
        from: &[TyId],
        to: &[TyId],
        generics: u32,
        span: Span,
    ) -> Adapter {
        // A generic method called through the adapter would need the call's type arguments
        // forwarded: checked once every body is (`generic_override_sites::check_adapters`).
        self.cx.adapter_uses.push((iface, span));
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
            generics as usize,
            self.module,
        );
        let params = (0..generics).map(|i| self.cx.ty.param(i)).collect();
        let self_ty = self.cx.ty.intern(TyKind::Adt(d, params));
        // A counted box: a share of the converted value is another reference to the same
        // adapter, so every copy reaches the one wrapped value (a copy of the adapter would
        // clone it, and a class instance with it).
        if !self.cx.counted_objects.contains(&self_ty) {
            self.cx.counted_objects.push(self_ty);
        }
        self.cx.impls.push(ImplDef {
            ty: self_ty,
            generics,
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
            generics,
            module: self.module,
            span,
        };
        let methods = sigs
            .iter()
            .enumerate()
            .map(|(slot, s)| match s.own {
                0 => adapter_method(self.cx, &parts, slot as u32, s, false),
                own => super::iface_adapt_slots::adapter_stub(self.cx, &parts, s, own),
            })
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
        // An argument for a `never` parameter: no value is one, so the call stops with a
        // `TypeError`, as a failed conversion of an `any` does.
        if to == self.cx.ty.never && self.cx.dynamic(h.ty).is_some() {
            let span = h.span;
            let msg = self.str_lit(
                "TypeError: a value was passed where the interface takes `never`",
                span,
            );
            return self.intrinsic(hir::Intrinsic::Panic, vec![msg], to, span);
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

    /// An array of interface values `h` as the array type `exp` of other instances of the
    /// interface, inside an adapter (a field `kids: Tree<T>[]` read through `Tree<any>`):
    /// `{ const src = h; const f = (e) => e; src.map(f) }`, the arrow checked against
    /// `(e: A) => B`, which adapts each element. `Err` for other types.
    pub(super) fn adapt_array(&mut self, h: hir::Expr, exp: TyId) -> Result<hir::Expr, hir::Expr> {
        let (TyKind::Array(a), TyKind::Array(b)) =
            (self.cx.ty.kind(h.ty).clone(), self.cx.ty.kind(exp).clone())
        else {
            return Err(h);
        };
        let (TyKind::Dyn(i, _), TyKind::Dyn(j, _)) = (self.cx.ty.kind(a), self.cx.ty.kind(b))
        else {
            return Err(h);
        };
        if i != j {
            return Err(h);
        }
        let span = h.span;
        let never = self.cx.ty.never;
        let fn_ty = self.cx.ty.intern(TyKind::FnPtr {
            params: vec![a],
            ret: b,
            throws: never,
            rest: false,
        });
        let n = self.f.locals.len();
        let ident = |name: String| ast::Ident { name, span };
        let (src_name, f_name, e_name) = (
            ident(format!("#adaptsrc{n}")),
            ident(format!("#adaptf{n}")),
            ident(format!("#adapte{n}")),
        );
        let var = |id: &ast::Ident| synth(ast::ExprKind::Ident(id.clone()), span);
        self.push_scope();
        let src = self.declare_local(&src_name, h.ty, LocalKind::Const);
        let arrow = synth(
            ast::ExprKind::Arrow {
                type_params: vec![],
                params: vec![ast::ArrowParam {
                    name: e_name.clone(),
                    pattern: None,
                    ty: None,
                    default: None,
                    optional: false,
                    rest: false,
                }],
                ret: None,
                throws: None,
                body: ast::ArrowBody::Expr(Box::new(var(&e_name))),
                is_async: false,
            },
            span,
        );
        let w = self.closure(&arrow, Some(fn_ty), true);
        let f = self.declare_local(&f_name, w.ty, LocalKind::Const);
        let map = synth(
            ast::ExprKind::Call {
                callee: Box::new(synth(
                    ast::ExprKind::Member {
                        object: Box::new(var(&src_name)),
                        prop: ident("map".into()),
                        optional: false,
                    },
                    span,
                )),
                type_args: vec![],
                args: vec![var(&f_name)],
                optional: false,
            },
            span,
        );
        let call = self.expr(&map, Some(exp), Want::Move);
        self.pop_scope();
        if call.ty != exp {
            return Err(self.error_expr(span));
        }
        let mut h = h;
        self.force_move(&mut h);
        let stmt = |local, init| hir::Stmt {
            kind: hir::StmtKind::Let {
                local,
                init: Some(init),
            },
            span,
        };
        let block = hir::Block {
            stmts: vec![stmt(src, h), stmt(f, w)],
            value: Some(Box::new(call)),
            span,
        };
        Ok(self.mk(H::Block(block), exp, span))
    }

    /// A function value `h` as function type `exp` inside an adapter: `{ const f = h; (p…) =>
    /// f(p…) }`, the arrow checked against `exp`, which converts each argument and the result.
    pub(super) fn adapt_fn(&mut self, h: hir::Expr, exp: TyId) -> Result<hir::Expr, hir::Expr> {
        self.adapt_fn_as(h, exp, false)
    }

    /// [`Self::adapt_fn`]; `undefined`: the function returns nothing and the arrow returns
    /// `undefined` after calling it (`(p…) => { f(p…); return undefined; }`), for a callback
    /// whose `void` result is a member of a union (`then(f, g)`, TypeScript's `T | void`).
    pub(super) fn adapt_fn_as(
        &mut self,
        h: hir::Expr,
        exp: TyId,
        undefined: bool,
    ) -> Result<hir::Expr, hir::Expr> {
        let TyKind::FnPtr { params, rest, .. } = self.cx.ty.kind(exp) else {
            return Err(h);
        };
        let shape = Shape::of(params.len(), *rest);
        // A function taking fewer parameters than `exp` gets the first ones (#943).
        let plan = match self.cx.ty.kind(h.ty) {
            TyKind::FnPtr {
                params: own,
                rest: false,
                ..
            } if own.len() < shape.fixed => Plan::one((0..own.len()).map(Arg::Param).collect()),
            _ => shape.same(),
        };
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
                    &plan,
                    false,
                    span,
                );
                let arrow = undefined_after(arrow, undefined, span);
                let mut w = self.closure(&arrow, Some(exp), false);
                self.mark_converts(&mut w, self.local_ty(l), exp);
                self.pop_scope();
                return Ok(w);
            }
        }
        let local = self.declare_local(&name, h.ty, LocalKind::Const);
        // A closure literal adapted: calls through the hidden local are calls of it.
        if let H::Closure(c) = &h.kind {
            self.f.closure_consts.insert(local, *c);
        }
        let arrow = wrapper(
            synth(ast::ExprKind::Ident(name), span),
            shape,
            &plan,
            false,
            span,
        );
        let arrow = undefined_after(arrow, undefined, span);
        let mut w = self.closure(&arrow, Some(exp), true);
        self.mark_converts(&mut w, h.ty, exp);
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

/// The wrapper arrow `(p…) => f(p…)` as `(p…) => { f(p…); return undefined; }` when `undefined`.
fn undefined_after(arrow: ast::Expr, undefined: bool, span: velt_common::Span) -> ast::Expr {
    let mut arrow = arrow;
    if let (true, ast::ExprKind::Arrow { body, .. }) = (undefined, &mut arrow.kind) {
        if let ast::ArrowBody::Expr(call) = body {
            let call = (**call).clone();
            let undef = synth(ast::ExprKind::Lit(ast::Lit::Undefined), span);
            let stmt = |kind| ast::Stmt { kind, span };
            *body = ast::ArrowBody::Block(ast::Block {
                stmts: vec![
                    stmt(ast::StmtKind::Expr(call)),
                    stmt(ast::StmtKind::Return(Some(undef))),
                ],
                span,
            });
        }
    }
    arrow
}
