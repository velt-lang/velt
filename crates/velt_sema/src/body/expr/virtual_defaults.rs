//! Parameter defaults of a virtual call (`x.m()` on a `B` holding a `D` whose override has
//! its own default): in JavaScript a default belongs to the function that runs, so a left-out
//! argument (or one that is `undefined`) takes the default of the override the call reaches.
//! Such a call goes through the method's default-filling slot (`collect::fill_defaults`): the
//! arguments those parameters get are `T | undefined` (`undefined` where left out), and the
//! entry of the receiver's class applies its own default where one is `undefined`. A call
//! passing every argument, or one whose receiver's class has no override below it, calls the
//! method's own slot as before.
//!
//! The entries' bodies are made here ([`build_fill_bodies`]), before the function bodies are
//! checked, with each override's defaults checked as its callers' would be.

use velt_common::Span;
use velt_syntax::ast;

use super::args::Callable;
use crate::body::{FnCx, Frame, LocalKind, Want};
use crate::collect::fill_defaults::fill_key;
use crate::ctx::Ctx;
use crate::defs::FnKind;
use crate::hir::{self, Callee, Def, DefId, ExprKind as H, Nothing, PassMode, TyId, UseMode};
use crate::resolve::TyEnv;

/// The default-filling slot a call uses: its slot, the receiver class's entry there, the
/// parameters it takes as `T | undefined`, and for one of left-out trailing arguments the
/// number of arguments it takes (`collect::fill_defaults`).
pub(super) struct FillTarget {
    pub slot: u32,
    pub entry: DefId,
    pub positions: Vec<usize>,
    pub trailing: Option<usize>,
}

impl FnCx<'_, '_> {
    /// The fill slot for a call of method `def` (vtable `slot` of the class of `recv_ty`) when
    /// the class has one, an override below it may run, and `args` (as written) leaves
    /// out an argument of one of its parameters, passes `undefined` there, or a value that may
    /// be `undefined` (a variable, a call's result, a method value's arrow, `method_value.rs`).
    /// A call that only leaves out trailing arguments takes the slot that applies the defaults
    /// of the rest.
    pub(super) fn fill_target(
        &mut self,
        recv_ty: TyId,
        def: DefId,
        slot: u32,
        args: &[ast::Expr],
    ) -> Option<FillTarget> {
        let t = self.fill_slot(recv_ty, def, slot, None)?;
        let written = args.len();
        let passed: Vec<bool> = (t.positions.clone().into_iter())
            .map(|k| k < written && self.maybe_undefined(&args[k]))
            .collect();
        let left_out = t.positions.iter().any(|&k| k >= written);
        if !passed.contains(&true) && left_out {
            if let Some(tt) = self.fill_slot(recv_ty, def, slot, Some(written)) {
                return Some(tt);
            }
        }
        (left_out || passed.contains(&true)).then_some(t)
    }

    /// May argument `a` be `undefined`: the literal, or a value whose checked type holds it
    /// (checked as a trial; it is checked again as the argument)?
    fn maybe_undefined(&mut self, a: &ast::Expr) -> bool {
        if crate::body::defaults::is_undefined(a) {
            return true;
        }
        let ty = match self.peek_ty(a) {
            Some(t) => Some(t),
            None if super::method_value::is_path(a) => None,
            None => self.trial_type(a),
        };
        ty.is_some_and(|t| self.cx.ty.nothing_of(t) == Some(Nothing::Undefined))
    }

    /// The fill slot of method `def` (vtable `slot`) in the class of `recv_ty` (the general
    /// one, or the one taking `trailing` arguments), when an override below that class may run.
    pub(super) fn fill_slot(
        &mut self,
        recv_ty: TyId,
        def: DefId,
        slot: u32,
        trailing: Option<usize>,
    ) -> Option<FillTarget> {
        let (s, _) = self.adt_of(recv_ty)?;
        let a = self.cx.adt(s)?;
        let key = a.vslots.iter().find(|(_, &v)| v == slot)?.0;
        let fslot = *a.vslots.get(&fill_key(key, trailing))?;
        let entry = *a.vtable.get(fslot as usize)?;
        let positions = self.cx.fills.get(&entry)?.positions.clone();
        let below = self.classes_below(s, slot);
        if below.iter().all(|&(_, m)| m == def) {
            return None;
        }
        Some(FillTarget {
            slot: fslot,
            entry,
            positions,
            trailing,
        })
    }

    /// Callable `c` of the method with the parameters of fill target `t`: the first `k` for
    /// one of left-out trailing arguments, else those at its positions taking `T | undefined`,
    /// `undefined` when left out.
    pub(super) fn fill_callable(&mut self, c: &Callable, t: &FillTarget, span: Span) -> Callable {
        let mut c = c.clone();
        if let Some(k) = t.trailing {
            c.params.truncate(k);
            return c;
        }
        let undefined = super::setters::synth(ast::ExprKind::Lit(ast::Lit::Undefined), span);
        for &k in &t.positions {
            let Some(ty) = c.params.get(k).map(|p| p.ty) else {
                continue;
            };
            let opt = self.cx.ty.option_of(ty, Nothing::Undefined);
            let d = self.expr_coerce(&undefined, opt, Want::Move);
            let p = &mut c.params[k];
            p.ty = opt;
            p.default = Some(d);
        }
        c
    }

    /// The classes below class `s`, deepest first, each with the method its vtable calls at
    /// `slot` (computed once per class and slot).
    fn classes_below(&mut self, s: DefId, slot: u32) -> crate::ctx::ClassesBelow {
        if let Some(b) = self.cx.classes_below.get(&(s, slot)) {
            return b.clone();
        }
        let mut below: Vec<(DefId, DefId)> = vec![];
        for i in 0..self.cx.info.len() as u32 {
            let c = DefId(i);
            let Some(a) = self.cx.adt(c).filter(|a| a.kind == hir::AdtKind::Class) else {
                continue;
            };
            let Some(&m) = a.vtable.get(slot as usize) else {
                continue;
            };
            if c != s && self.class_extends(c, s) {
                below.push((c, m));
            }
        }
        let below: crate::ctx::ClassesBelow = below.into();
        self.cx.classes_below.insert((s, slot), below.clone());
        below
    }

    /// Does class `c` extend class `s`, directly or not?
    fn class_extends(&self, c: DefId, s: DefId) -> bool {
        let mut cur = self.cx.adt(c).and_then(|a| a.base);
        let mut hops = 0;
        while let Some((b, _)) = cur.and_then(|t| self.cx.class_of(t)) {
            if b == s {
                return true;
            }
            hops += 1;
            if hops > 256 {
                return false;
            }
            cur = self.cx.adt(b).and_then(|a| a.base);
        }
        false
    }
}

/// The bodies of the default-filling entries (`Ctx::fills`):
/// `return C.m(this, …, x === undefined ? <C.m's default> : x, …)`.
pub(crate) fn build_fill_bodies(cx: &mut Ctx) {
    let mut fills: Vec<(DefId, crate::collect::fill_defaults::Fill)> =
        cx.fills.iter().map(|(d, f)| (*d, f.clone())).collect();
    fills.sort_by_key(|(d, _)| d.0);
    for (def, fill) in fills {
        fill_body(cx, def, fill.method, &fill.positions, fill.trailing);
    }
}

fn fill_body(cx: &mut Ctx, def: DefId, m: DefId, positions: &[usize], trailing: Option<usize>) {
    crate::body::defaults::param_defaults(cx, m);
    let f = cx.fn_info(def);
    let (module, span, owner, name) = (f.module, f.span, f.owner, f.name.clone());
    let Some(this_sig) = f.this.clone() else {
        return;
    };
    let names = f.generics.names.clone();
    let params = f.params.clone();
    let ret = crate::body::returns::ret_of(cx, m, span);
    cx.fn_info_mut(def).ret = ret;
    let self_args: Vec<TyId> = (0..names.len() as u32).map(|i| cx.ty.param(i)).collect();
    let defaults: Vec<Option<hir::Expr>> = cx
        .fn_info(m)
        .params
        .iter()
        .map(|p| p.default.clone())
        .collect();
    let own_tys: Vec<TyId> = cx.fn_info(m).params.iter().map(|p| p.ty).collect();
    let env = TyEnv::new(module, &names);
    let mut g = FnCx::new(cx, module, env, Frame::new(FnKind::Method, Some(ret)));
    g.fn_name = name.clone();
    g.owner = owner;
    g.body_def = Some(def);
    g.cx.checking.push(def);
    let this_use = match this_sig.mode {
        PassMode::BorrowMut => UseMode::BorrowMut,
        _ => UseMode::Borrow,
    };
    let this = g.new_local("this", this_sig.ty, false, span, LocalKind::This);
    let mut hir_params = vec![hir::Param {
        local: this,
        ty: this_sig.ty,
        mode: this_sig.mode,
    }];
    let mut args = vec![g.mk(H::Local(this, this_use), this_sig.ty, span)];
    for (k, p) in params.iter().enumerate() {
        let l = g.new_local(&p.name, p.ty, false, span, LocalKind::Param);
        hir_params.push(hir::Param {
            local: l,
            ty: p.ty,
            mode: p.mode,
        });
        let want = match p.mode {
            PassMode::Owned => Want::Move,
            _ => Want::Borrow,
        };
        let mode = g.use_mode(p.ty, want);
        let read = g.mk(H::Local(l, mode), p.ty, span);
        let arg = match (positions.contains(&k), defaults.get(k).cloned().flatten()) {
            (true, Some(d)) => {
                let target = own_tys.get(k).copied().unwrap_or(p.ty);
                match g.default_where_undefined(read, target, &d, &self_args) {
                    Ok(e) | Err(e) => e,
                }
            }
            _ => read,
        };
        args.push(arg);
    }
    // The parameters a trailing entry does not take: the method's own defaults.
    for d in defaults
        .iter()
        .skip(params.len())
        .take_while(|_| trailing.is_some())
    {
        let arg = match d {
            Some(d) => g.placed_default(d, &self_args, span),
            None => g.unit_expr(span),
        };
        args.push(arg);
    }
    g.call_throws(m, &self_args, ret, span);
    let call = g.mk(
        H::Call {
            callee: Callee::Def(m, self_args),
            args,
        },
        ret,
        span,
    );
    let unit = g.cx.ty.unit;
    let kind = match ret == unit {
        true => hir::StmtKind::Expr(call),
        false => hir::StmtKind::Return(Some(call)),
    };
    g.cx.checking.pop();
    let frame = std::mem::replace(&mut g.f, Frame::new(FnKind::Method, None));
    drop(g);
    let fndef = hir::FnDef {
        name,
        generics: names.len() as u32,
        params: hir_params,
        ret,
        is_async: false,
        is_generator: false,
        self_ty: Some(this_sig.ty),
        captures: vec![],
        shares_captures: false,
        body: hir::Body {
            locals: frame.locals,
            block: hir::Block {
                stmts: vec![hir::Stmt { kind, span }],
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
}
