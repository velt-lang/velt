//! The methods of an interface adapter (`iface_adapt`): one per slot of the interface, each
//! calling the same slot of the wrapped value with its arguments and result converted, and the
//! setter slots added once a field of the interface is written through (`iface_writes`).

use velt_common::Span;
use velt_syntax::ast;

use super::callback::synth;
use super::iface_adapt::{container, value_ok, Container, Mode, SlotSig};
use crate::body::{FnCx, Frame, LocalKind, Want};
use crate::ctx::Ctx;
use crate::defs::{BodyState, DefInfo, FnKind, Generics, ParamSig, ThisSig, ThrowSrc};
use crate::hir::{self, Callee, Def, DefId, ExprKind as H, PassMode, TyId, TyKind, UseMode};
use crate::resolve::TyEnv;

/// What an adapter's methods are made from.
#[derive(Clone, Debug)]
pub(crate) struct AdapterParts {
    pub def: DefId,
    pub self_ty: TyId,
    pub inner: TyId,
    pub iface: DefId,
    pub iname: String,
    pub from: Vec<TyId>,
    pub to: Vec<TyId>,
    /// The adapter's type parameters (`T0..`), which `from` is over (`iface_adapt_dynamic`).
    pub generics: u32,
    pub module: usize,
    pub span: Span,
}

impl AdapterParts {
    /// The names of the adapter's type parameters.
    fn param_names(&self) -> Vec<String> {
        (0..self.generics).map(|i| format!("T{i}")).collect()
    }
}

/// The method of the adapter for slot `slot` (signature `s`): calls the slot on the wrapped
/// value, converting the arguments and the result. A `setter` slot takes `this` mutably and its
/// value by move, as the setters of `iface_writes` do.
pub(super) fn adapter_method(
    cx: &mut Ctx,
    p: &AdapterParts,
    slot: u32,
    s: &SlotSig,
    setter: bool,
) -> DefId {
    let span = p.span;
    let (this_mode, this_use) = match setter {
        true => (PassMode::BorrowMut, UseMode::BorrowMut),
        false => (PassMode::Borrow, UseMode::Borrow),
    };
    let mut targs = p.to.clone();
    targs.push(p.self_ty);
    let mut fargs = p.from.clone();
    fargs.push(p.inner);
    let params: Vec<(ParamSig, TyId)> = s
        .params
        .iter()
        .map(|q| {
            let ty = cx.subst(q.ty, &targs);
            let mode = match (cx.is_copy(ty), setter) {
                (true, _) => PassMode::Copy,
                (false, true) => PassMode::Owned,
                (false, false) => PassMode::Borrow,
            };
            let from = cx.subst(q.ty, &fargs);
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
    let ret = cx.subst(s.ret, &targs);
    let ret_from = cx.subst(s.ret, &fargs);
    let name = format!("{} adapter.{}", p.iname, s.name);
    let mut info =
        crate::collect::fn_placeholder(name.clone(), span, span, p.module, FnKind::Method, None);
    let names = p.param_names();
    let mut generics = Generics::default();
    for n in &names {
        generics.push(n);
    }
    info.generics = generics;
    info.this = Some(ThisSig {
        ty: p.self_ty,
        mode: this_mode,
    });
    info.params = params.iter().map(|(q, _)| q.clone()).collect();
    info.ret = ret;
    info.fixed_modes = true;
    info.state = BodyState::Done;
    info.owner = Some(p.def);
    let def = cx.alloc_def(span, DefInfo::Fn(Box::new(info)));
    let env = TyEnv::new(p.module, &names);
    let mut g = FnCx::new(cx, p.module, env, Frame::new(FnKind::Method, Some(ret)));
    g.adapting = true;
    g.fn_name = name.clone();
    g.owner = Some(p.def);
    g.body_def = Some(def);
    g.cx.checking.push(def);
    let this = g.new_local("this", p.self_ty, false, span, LocalKind::This);
    let mut args = vec![hir::Expr {
        kind: H::Field {
            base: Box::new(g.mk(H::Local(this, this_use), p.self_ty, span)),
            index: 0,
            mode: this_use,
        },
        ty: p.inner,
        span,
    }];
    let mut hir_params = vec![hir::Param {
        local: this,
        ty: p.self_ty,
        mode: this_mode,
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
            Some(c) if !setter && g.cx.dynamic(q.ty).is_some() => Some(c),
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
        let mode = g.use_mode(q.ty, if setter { Want::Move } else { Want::Borrow });
        let read = g.mk(H::Local(l, mode), q.ty, span);
        args.push(g.adapt_value(read, *from));
    }
    if !promise_slot(g.cx, p.iface, slot) {
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
        generics: p.generics,
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
    cx.defs[def.0 as usize] = Some(Def::Fn(fndef));
    let info = cx.fn_info_mut(def);
    info.local_kinds = frame.kinds;
    info.throw_srcs = frame.uncaught;
    def
}

/// Is slot `slot` of `iface` one whose implementations reject a promise instead of throwing
/// (`throws/groups.rs`)?
fn promise_slot(cx: &mut Ctx, iface: DefId, slot: u32) -> bool {
    let groups = cx.throw_groups();
    groups
        .slot_group(iface, slot)
        .is_some_and(|g| groups.list[g].promise)
}

/// The setter slot of field `field` for the adapter `p` (`iface_writes`, a field written through
/// the interface): writes the value, converted to the wrapped value's own field type, through
/// the wrapped value's setter. `None` when the value does not convert back (a field of an
/// interface type at `unknown`, #924).
pub(crate) fn adapter_setter(cx: &mut Ctx, p: &AdapterParts, field: usize) -> Option<DefId> {
    let info = cx.iface(p.iface)?;
    let f = info.fields.get(field)?.clone();
    let slot = cx.setter_slot(p.iface, field)?;
    let mut targs = p.to.clone();
    targs.push(p.self_ty);
    let mut fargs = p.from.clone();
    fargs.push(p.inner);
    let (to, from) = (cx.subst(f.ty, &targs), cx.subst(f.ty, &fargs));
    let mut m = Mode {
        generic: p.generics > 0,
        ..Mode::default()
    };
    if !cx.ty.has_error(to) && !value_ok(cx, to, from, false, &mut m) {
        return None;
    }
    let sig = SlotSig {
        name: format!("<set {}>", f.name),
        params: vec![ParamSig {
            name: "v".into(),
            span: p.span,
            ty: f.ty,
            mode: PassMode::Owned,
            default: None,
        }],
        ret: cx.ty.unit,
    };
    Some(adapter_method(cx, p, slot, &sig, true))
}
