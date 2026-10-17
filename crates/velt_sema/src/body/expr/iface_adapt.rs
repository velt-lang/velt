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

use super::callback::{synth, wrapper};
use crate::any::Dynamic;
use crate::body::{FnCx, Frame, LocalKind, Want};
use crate::ctx::Ctx;
use crate::defs::{BodyState, DefInfo, FnKind, ParamSig, ThisSig, ThrowSrc};
use crate::hir::{
    self, Callee, Def, DefId, ExprKind as H, ImplDef, PassMode, TyId, TyKind, UseMode,
};
use crate::resolve::TyEnv;

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
struct SlotSig {
    name: String,
    params: Vec<ParamSig>,
    ret: TyId,
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
enum Container {
    Array,
    Map,
    Set,
}

impl Container {
    /// The prelude helpers making the copy and writing it back.
    fn helpers(self) -> (&'static str, &'static str) {
        match self {
            Container::Array => ("__unknownArrayIn", "__unknownArrayOut"),
            Container::Map => ("__unknownMapIn", "__unknownMapOut"),
            Container::Set => ("__unknownSetIn", "__unknownSetOut"),
        }
    }
}

/// Is `t` one of the [`Container`] types?
fn container(cx: &Ctx, t: TyId) -> Option<Container> {
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
    sigs.iter().all(|s| {
        let (rf, rt) = (cx.subst(s.ret, &fa), cx.subst(s.ret, &ta));
        value_ok(cx, rf, rt, false, seen, why)
            && s.params.iter().all(|p| {
                let (pf, pt) = (cx.subst(p.ty, &fa), cx.subst(p.ty, &ta));
                value_ok(cx, pt, pf, true, seen, why)
            })
    })
}

/// Does a value of type `from` convert to `to` inside an adapter? `param` when it is a
/// parameter of the slot itself, where a [`Container`] is passed as a copy.
fn value_ok(
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
            },
            TyKind::FnPtr {
                params: q,
                ret: s,
                throws: u,
            },
        ) => {
            p.len() == q.len()
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
            span,
        };
        let methods = sigs
            .iter()
            .enumerate()
            .map(|(slot, s)| self.adapter_method(&parts, slot as u32, s))
            .collect();
        self.cx.impls[a.impl_index as usize].methods = methods;
        // The dispatch groups gain the new methods (`throws/groups.rs`): built again on next use.
        self.cx.groups = None;
        a
    }

    /// The method of the adapter for slot `slot` (signature `s`): calls the slot on the wrapped
    /// value, converting the arguments and the result.
    fn adapter_method(&mut self, p: &AdapterParts, slot: u32, s: &SlotSig) -> DefId {
        let span = p.span;
        let mut targs = p.to.clone();
        targs.push(p.self_ty);
        let mut fargs = p.from.clone();
        fargs.push(p.inner);
        let params: Vec<(ParamSig, TyId)> = s
            .params
            .iter()
            .map(|q| {
                let ty = self.cx.subst(q.ty, &targs);
                let mode = if self.cx.is_copy(ty) {
                    PassMode::Copy
                } else {
                    PassMode::Borrow
                };
                let from = self.cx.subst(q.ty, &fargs);
                (
                    ParamSig {
                        ty,
                        mode,
                        ..q.clone()
                    },
                    from,
                )
            })
            .collect();
        let ret = self.cx.subst(s.ret, &targs);
        let ret_from = self.cx.subst(s.ret, &fargs);
        let name = format!("{} adapter.{}", p.iname, s.name);
        let mut info = crate::collect::fn_placeholder(
            name.clone(),
            span,
            span,
            self.module,
            FnKind::Method,
            None,
        );
        info.this = Some(ThisSig {
            ty: p.self_ty,
            mode: PassMode::Borrow,
        });
        info.params = params.iter().map(|(q, _)| q.clone()).collect();
        info.ret = ret;
        info.fixed_modes = true;
        info.state = BodyState::Done;
        info.owner = Some(p.def);
        let def = self.cx.alloc_def(span, DefInfo::Fn(Box::new(info)));
        let env = TyEnv::new(self.module, &[]);
        let mut g = FnCx::new(
            self.cx,
            self.module,
            env,
            Frame::new(FnKind::Method, Some(ret)),
        );
        g.adapting = true;
        g.fn_name = name.clone();
        g.owner = Some(p.def);
        g.body_def = Some(def);
        g.cx.checking.push(def);
        let this = g.new_local("this", p.self_ty, false, span, LocalKind::This);
        let mut args = vec![hir::Expr {
            kind: H::Field {
                base: Box::new(g.mk(H::Local(this, UseMode::Borrow), p.self_ty, span)),
                index: 0,
                mode: UseMode::Borrow,
            },
            ty: p.inner,
            span,
        }];
        let mut hir_params = vec![hir::Param {
            local: this,
            ty: p.self_ty,
            mode: PassMode::Borrow,
        }];
        // A container argument is passed as a copy holding its elements boxed, and the copy is
        // written back when the call returns (`Container`).
        let mut stmts = vec![];
        let mut write_back = vec![];
        for (k, ((q, from), orig)) in params.iter().zip(&s.params).enumerate() {
            let l = g.new_local(&q.name, q.ty, false, span, LocalKind::Param);
            hir_params.push(hir::Param {
                local: l,
                ty: q.ty,
                mode: q.mode,
            });
            let boxed = match container(g.cx, *from) {
                Some(c) if g.cx.dynamic(q.ty).is_some() => Some(c),
                _ => None,
            };
            if let Some(c) = boxed {
                let (copy_in, copy_out) = c.helpers();
                let (pname, cname) = (format!("#param{k}"), format!("#copy{k}"));
                if let Some(scope) = g.f.scopes.last_mut() {
                    scope.names.insert(pname.clone(), l);
                }
                let ident = |n: &str| {
                    synth(
                        ast::ExprKind::Ident(ast::Ident {
                            name: n.to_string(),
                            span,
                        }),
                        span,
                    )
                };
                let call_of = |f: &str, args: Vec<ast::Expr>| {
                    synth(
                        ast::ExprKind::Call {
                            callee: Box::new(ident(f)),
                            type_args: vec![],
                            args,
                            optional: false,
                        },
                        span,
                    )
                };
                // The helpers for `Set` are in its own module (std/collections/set.vlt), which
                // the module converting the value need not import: their calls are checked as
                // if written there.
                let helpers_module = match g.cx.ty.kind(*from) {
                    TyKind::Adt(d, _) if c == Container::Set => {
                        g.cx.adt(*d).map_or(g.module, |a| a.module)
                    }
                    _ => g.module,
                };
                let own_module = std::mem::replace(&mut g.module, helpers_module);
                let init = g.expr(
                    &call_of(copy_in, vec![ident(&pname)]),
                    Some(*from),
                    Want::Move,
                );
                g.module = own_module;
                let ci = ast::Ident {
                    name: cname.clone(),
                    span,
                };
                let copy = g.declare_local(&ci, *from, LocalKind::Const);
                stmts.push(hir::Stmt {
                    kind: hir::StmtKind::Let {
                        local: copy,
                        init: Some(init),
                    },
                    span,
                });
                let read = g.mk(H::Local(copy, UseMode::Borrow), *from, span);
                args.push(if orig.mode == PassMode::Borrow {
                    read
                } else {
                    g.intrinsic(hir::Intrinsic::Share, vec![read], *from, span)
                });
                write_back.push((
                    helpers_module,
                    call_of(copy_out, vec![ident(&pname), ident(&cname)]),
                ));
                // The helpers read and write through `any` (`Program::dyn_members`).
                g.cx.note_dyn_members();
                continue;
            }
            let mode = g.use_mode(q.ty, Want::Borrow);
            let read = g.mk(H::Local(l, mode), q.ty, span);
            args.push(g.adapt_value(read, *from));
        }
        if !g.promise_slot(p.iface, slot) {
            g.throw_src(ThrowSrc::Slot {
                iface: p.iface,
                slot,
                args: fargs,
                span,
            });
        }
        let call = g.mk(
            H::Call {
                callee: Callee::Dyn { slot },
                args,
            },
            ret_from,
            span,
        );
        let value = g.adapt_value(call, ret);
        let unit = g.cx.ty.unit;
        let stmt = |kind| hir::Stmt { kind, span };
        let result = if ret == unit {
            stmts.push(stmt(hir::StmtKind::Expr(value)));
            None
        } else if write_back.is_empty() {
            Some(value)
        } else {
            // The result is kept while the copies are written back.
            let r = g.new_local("#result", ret, false, span, LocalKind::Temp);
            stmts.push(stmt(hir::StmtKind::Let {
                local: r,
                init: Some(value),
            }));
            let mode = g.use_mode(ret, Want::Move);
            Some(g.mk(H::Local(r, mode), ret, span))
        };
        for (module, w) in &write_back {
            let own_module = std::mem::replace(&mut g.module, *module);
            let h = g.expr(w, Some(unit), Want::Move);
            g.module = own_module;
            stmts.push(stmt(hir::StmtKind::Expr(h)));
        }
        if let Some(v) = result {
            stmts.push(stmt(hir::StmtKind::Return(Some(v))));
        }
        g.cx.checking.pop();
        let frame = std::mem::replace(&mut g.f, Frame::new(FnKind::Method, None));
        drop(g);
        let fndef = hir::FnDef {
            name,
            generics: 0,
            params: hir_params,
            ret,
            is_async: false,
            is_generator: false,
            self_ty: Some(p.self_ty),
            captures: vec![],
            shares_captures: false,
            body: hir::Body {
                locals: frame.locals,
                block: hir::Block {
                    stmts,
                    value: None,
                    span,
                },
            },
            throws: None,
            span,
        };
        self.cx.defs[def.0 as usize] = Some(Def::Fn(fndef));
        let info = self.cx.fn_info_mut(def);
        info.local_kinds = frame.kinds;
        info.throw_srcs = frame.uncaught;
        def
    }

    /// Is slot `slot` of `iface` one whose implementations reject a promise instead of throwing
    /// (`throws/groups.rs`)?
    fn promise_slot(&mut self, iface: DefId, slot: u32) -> bool {
        let groups = self.cx.throw_groups();
        groups
            .slot_group(iface, slot)
            .is_some_and(|g| groups.list[g].promise)
    }

    /// `h` converted to `to` inside an adapter's method (`adapting`): unchanged, checked out of
    /// `unknown`, boxed, or wrapped.
    fn adapt_value(&mut self, h: hir::Expr, to: TyId) -> hir::Expr {
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
        let TyKind::FnPtr { params, .. } = self.cx.ty.kind(exp) else {
            return Err(h);
        };
        let n = params.len();
        let span = h.span;
        let name = ast::Ident {
            name: format!("#adapted{}", self.f.locals.len()),
            span,
        };
        self.push_scope();
        if let H::Local(l, _) = h.kind {
            if self.local_kind(l) == LocalKind::Param {
                // A function passed to the method is borrowed: the arrow calling it is passed on
                // at once and never kept, so it borrows the parameter by name.
                if let Some(scope) = self.f.scopes.last_mut() {
                    scope.names.insert(name.name.clone(), l);
                }
                let arrow = wrapper(synth(ast::ExprKind::Ident(name), span), n, n, false, span);
                let w = self.closure(&arrow, Some(exp), false);
                self.pop_scope();
                return Ok(w);
            }
        }
        let local = self.declare_local(&name, h.ty, LocalKind::Const);
        let arrow = wrapper(synth(ast::ExprKind::Ident(name), span), n, n, false, span);
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

/// What an adapter's methods are made from.
struct AdapterParts {
    def: DefId,
    self_ty: TyId,
    inner: TyId,
    iface: DefId,
    iname: String,
    from: Vec<TyId>,
    to: Vec<TyId>,
    span: Span,
}
