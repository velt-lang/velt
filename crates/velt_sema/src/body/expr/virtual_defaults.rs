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

use std::borrow::Cow;

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
    /// the class has one, an override below it may run, and `args` (spreads expanded) leaves
    /// out an argument of one of its parameters, passes `undefined` there, or a value that may
    /// be `undefined` (a variable, a call's result, a method value's arrow, `method_value.rs`).
    /// A call that only leaves out trailing arguments (or passes `undefined` literals for them:
    /// `m(i, undefined)` is `m(i)`, but not for a rest parameter) takes the slot that applies
    /// the defaults of the rest, which takes the arguments before them only.
    pub(super) fn fill_target(
        &mut self,
        recv_ty: TyId,
        def: DefId,
        slot: u32,
        args: &[ast::Expr],
    ) -> Option<FillTarget> {
        let t = self.fill_slot(recv_ty, def, slot, None)?;
        let is_undefined = crate::body::defaults::is_undefined;
        // Only the `undefined`s standing for named parameters can be left out: one passed to a
        // rest parameter is an element of it (and those before it are then passed too).
        let rest_at = (crate::body::defaults::has_rest(self.cx, def))
            .then(|| self.cx.fn_info(def).params.len().saturating_sub(1));
        let written = match rest_at {
            Some(r) if args.len() > r => args.len(),
            _ => args.len() - args.iter().rev().take_while(|a| is_undefined(a)).count(),
        };
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

    /// The fill target of a virtual call of method `def` (vtable `slot`, callable `c`) on a
    /// `recv_ty` (`fill_target`) and the arguments it passes: spreads of a known length (a
    /// tuple, an array literal, `apply`'s list) stand for their elements, and the `undefined`s
    /// a trailing entry leaves out are dropped. A spread of an unknown length before a
    /// parameter whose default an override changes is reported (`Err`).
    pub(super) fn virtual_fill<'e>(
        &mut self,
        c: &Callable,
        recv_ty: TyId,
        (def, slot): (DefId, u32),
        args: &'e [ast::Expr],
        span: Span,
    ) -> Result<(Option<FillTarget>, Cow<'e, [ast::Expr]>), ()> {
        let Some(general) = self.fill_slot(recv_ty, def, slot, None) else {
            return Ok((None, Cow::Borrowed(args)));
        };
        let mut args = Cow::Borrowed(args);
        let is_spread = |a: &ast::Expr| matches!(a.kind, ast::ExprKind::Spread(_));
        if args.iter().any(is_spread) {
            if let Some(a) = self.expand_spreads(c, &args, span)? {
                args = Cow::Owned(a);
            }
        }
        if let Some(i) = args.iter().position(is_spread) {
            if let Some(&k) = general.positions.iter().find(|&&k| k >= i) {
                self.spread_before_fill(c, k, &args[i]);
                let others: Vec<ast::Expr> =
                    args.iter().filter(|a| !is_spread(a)).cloned().collect();
                self.check_args_loose(&others);
                return Err(());
            }
        }
        let fill = self.fill_target(recv_ty, def, slot, &args);
        if let Some(k) = fill.as_ref().and_then(|t| t.trailing) {
            if k < args.len() {
                args = Cow::Owned(args[..k].to_vec());
            }
        }
        Ok((fill, args))
    }

    /// A spread of an unknown length `a` that may fill parameter `k`, whose default an override
    /// changes (reported).
    fn spread_before_fill(&mut self, c: &Callable, k: usize, a: &ast::Expr) {
        let name = c.params.get(k).map_or("", |p| p.name.as_str()).to_string();
        self.cx.error(
            velt_common::Diagnostic::error(
                format!("a spread argument that may fill parameter `{name}` of {} is not supported yet", c.what),
                a.span,
            )
            .with_note(format!("an override gives `{name}` another default, and which override runs and whether the spread gives `{name}` a value are only known at run time"))
            .with_note(format!("pass `{name}` (and the parameters before it) as separate arguments, then spread the rest")),
        );
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
            // A trailing entry stops before the rest parameter, if any: the call passes no
            // rest arguments, and the entry passes the empty array on.
            c.rest &= k >= c.params.len();
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
    let own_modes: Vec<PassMode> = cx.fn_info(m).params.iter().map(|p| p.mode).collect();
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
    // A default is computed into a local before the call, so the call's arguments are reads of
    // locals and `this` is passed as borrowed (no reference count taken for the call).
    let mut stmts = vec![];
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
                let e = match g.default_where_undefined(read, target, &d, &self_args) {
                    Ok(e) | Err(e) => e,
                };
                hoisted(&mut g, e, own_modes.get(k).copied(), &mut stmts)
            }
            _ => read,
        };
        args.push(arg);
    }
    // The parameters a trailing entry does not take: the method's own defaults.
    for (k, d) in (defaults.iter().enumerate())
        .skip(params.len())
        .take_while(|_| trailing.is_some())
    {
        let arg = match d {
            Some(d) => {
                let e = g.placed_default(d, &self_args, span);
                hoisted(&mut g, e, own_modes.get(k).copied(), &mut stmts)
            }
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
    stmts.push(hir::Stmt { kind, span });
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
}

/// `e`, an argument of the call in a fill entry, as a read of a local holding it (a `let`
/// added to `stmts`) unless it is a literal or a local's read; `mode` is how the method takes it.
fn hoisted(
    g: &mut FnCx,
    e: hir::Expr,
    mode: Option<PassMode>,
    stmts: &mut Vec<hir::Stmt>,
) -> hir::Expr {
    if matches!(e.kind, H::Lit(_) | H::Local(..)) {
        return e;
    }
    let (ty, span) = (e.ty, e.span);
    let l = g.new_local("<default>", ty, false, span, LocalKind::Temp);
    stmts.push(hir::Stmt {
        kind: hir::StmtKind::Let {
            local: l,
            init: Some(e),
        },
        span,
    });
    let want = match mode {
        Some(PassMode::Owned) => Want::Move,
        _ => Want::Borrow,
    };
    let use_mode = g.use_mode(ty, want);
    g.mk(H::Local(l, use_mode), ty, span)
}
