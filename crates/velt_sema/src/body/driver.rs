//! Checking order: field defaults, module constants, parameter defaults, then every function
//! body. Bodies are checked on demand (`ensure_body`) because a `catch (e)` needs the thrown
//! types of the functions called in its `try`, which are only known once their bodies are.

use velt_common::{Diagnostic, Span};

use super::{recursion, FnCx, Frame, LocalKind, Want};
use crate::ctx::Ctx;
use crate::defs::{BodyState, DefInfo, FnKind, FnSource, RetSource};
use crate::hir::{self, Def, DefId, ExprKind as H};
use crate::resolve::TyEnv;

pub(crate) fn check_bodies(cx: &mut Ctx) {
    let all: Vec<DefId> = (0..cx.info.len() as u32).map(DefId).collect();
    for &d in &all {
        field_defaults(cx, d);
    }
    for &d in &all {
        if cx.global(d).is_some() {
            ensure_global(cx, d);
        }
    }
    let fns = cx.fn_defs.clone();
    for &d in &fns {
        super::defaults::param_defaults(cx, d);
    }
    super::defaults::iface_defaults(cx);
    for d in fns {
        ensure_body(cx, d);
    }
    super::returns::check_deferred(cx);
}

/// A checker for an expression outside any function body (defaults, constants).
pub(super) fn detached<'a, 'm>(
    cx: &'a mut Ctx<'m>,
    module: usize,
    generics: &[String],
) -> FnCx<'a, 'm> {
    let env = TyEnv::new(module, generics);
    FnCx::new(cx, module, env, Frame::new(FnKind::Free, None))
}

/// Check the own field defaults of type `d` (once), recording what each may throw.
pub(crate) fn field_defaults(cx: &mut Ctx, d: DefId) {
    if cx.adt(d).is_none_or(|a| a.decl.is_none()) || !cx.field_defaults_checked.insert(d) {
        return;
    }
    let a = cx.adt(d).expect("ICE: adt");
    let (decl, module, names, start, qual) = (
        a.decl.expect("ICE: decl"),
        a.module,
        a.generics.names.clone(),
        a.own_fields_start,
        a.qual_name.clone(),
    );
    let fields = cx.adt(d).expect("ICE: adt").fields[start..].to_vec();
    let saved = std::mem::replace(&mut cx.display_params, names.clone());
    for (i, f) in fields.iter().enumerate() {
        let Some(ast_field) = decl.fields.iter().find(|af| af.name.name == f.name) else {
            continue;
        };
        let mut throws = vec![];
        let default = match &ast_field.default {
            Some(e) => {
                let mut fcx = detached(cx, module, &names);
                fcx.owner = Some(d);
                fcx.fn_name = format!("{qual}.{}", f.name);
                let h = fcx.expr_coerce(e, f.ty, Want::Move);
                throws = std::mem::take(&mut fcx.f.uncaught);
                Some(h)
            }
            None if f.optional => Some(hir::Expr {
                kind: H::Lit(hir::Lit::Null),
                ty: f.ty,
                span: f.span,
            }),
            None => None,
        };
        let field = &mut cx.adt_mut(d).fields[start + i];
        field.default = default;
        field.default_throws = throws;
    }
    cx.display_params = saved;
}

/// Check a module-level constant (on first use or in declaration order).
pub(crate) fn ensure_global(cx: &mut Ctx, d: DefId) {
    let Some(g) = cx.global(d) else { return };
    match g.state {
        BodyState::Done => return,
        BodyState::InProgress => {
            let span = g.span;
            cx.err("module-level constant refers to itself", span);
            return;
        }
        BodyState::Unchecked => {}
    }
    let (module, qual, owner) = (g.module, g.qual_name.clone(), g.src.owner);
    let (ann, init_expr, decl_span) = (g.src.ann, g.src.init, g.src.span);
    set_global_state(cx, d, BodyState::InProgress);
    let saved = std::mem::take(&mut cx.display_params);
    let mut fcx = detached(cx, module, &[]);
    fcx.fn_name = qual;
    fcx.owner = owner;
    let ann = ann.map(|t| fcx.cx.resolve_type(t, &TyEnv::new(module, &[])));
    let init = match (init_expr, ann) {
        (Some(e), Some(t)) => Some(fcx.expr_coerce(e, t, Want::Move)),
        (Some(e), None) => Some(fcx.expr(e, None, Want::Move)),
        (None, _) => {
            let what = if owner.is_some() {
                "static fields must be initialized"
            } else {
                "`const` declarations must be initialized"
            };
            fcx.cx.err(what, decl_span);
            None
        }
    };
    let ty = ann.or(init.as_ref().map(|i| i.ty)).unwrap_or(cx.ty.error);
    if let Some(i) = &init {
        if !is_const_expr(i) {
            cx.error(
                Diagnostic::error(
                    "module-level constants must be constant expressions",
                    i.span,
                )
                .with_note("use literals, or struct literals of constants"),
            );
        }
    }
    cx.display_params = saved;
    if let DefInfo::Global(g) = &mut cx.info[d.0 as usize] {
        g.ty = ty;
        g.init = init;
        g.state = BodyState::Done;
    }
}

fn set_global_state(cx: &mut Ctx, d: DefId, s: BodyState) {
    if let DefInfo::Global(g) = &mut cx.info[d.0 as usize] {
        g.state = s;
    }
}

fn is_const_expr(e: &hir::Expr) -> bool {
    match &e.kind {
        H::Lit(_) | H::Global(_) => true,
        H::Unary { expr, .. } | H::Cast(expr) | H::WrapSome(expr) => is_const_expr(expr),
        H::Binary { lhs, rhs, .. } | H::Logical { lhs, rhs, .. } => {
            is_const_expr(lhs) && is_const_expr(rhs)
        }
        H::AdtLit { fields: xs, .. } | H::Variant { args: xs, .. } | H::Tuple(xs) => {
            xs.iter().all(is_const_expr)
        }
        _ => false,
    }
}

/// Check `def`'s body now unless it is already checked or being checked. A body whose
/// inferred return type was needed before it was known is checked again (`body::recursion`).
pub(crate) fn ensure_body(cx: &mut Ctx, def: DefId) {
    let f = cx.fn_info(def);
    if f.state != BodyState::Unchecked {
        return;
    }
    let Some(src) = f.source else {
        cx.fn_info_mut(def).state = BodyState::Done;
        return;
    };
    let inferred = f.ret_source == RetSource::Body;
    cx.fn_info_mut(def).state = BodyState::InProgress;
    let mark = recursion::Mark::new(cx);
    let mut fndef = check_body(cx, def, src);
    if recursion::needs_second_pass(cx, def, &mark) {
        fndef = check_body(cx, def, src);
    }
    cx.defs[def.0 as usize] = Some(Def::Fn(fndef));
    cx.fn_info_mut(def).state = BodyState::Done;
    recursion::completed(cx, def, inferred);
}

fn check_body(cx: &mut Ctx, def: DefId, src: FnSource) -> hir::FnDef {
    cx.checking.push(def);
    let in_return = std::mem::take(&mut cx.rec.in_return);
    let names = cx.fn_info(def).generics.names.clone();
    let saved = std::mem::replace(&mut cx.display_params, names);
    let fndef = check_fn(cx, def, src);
    cx.display_params = saved;
    cx.rec.in_return = in_return;
    cx.checking.pop();
    fndef
}

fn check_fn(cx: &mut Ctx, def: DefId, src: FnSource) -> hir::FnDef {
    // An unannotated override takes the result of the method it overrides.
    let declared = match cx.fn_info(def).ret_source {
        RetSource::Body => None,
        _ => Some(super::returns::ret_of(cx, def, cx.fn_info(def).name_span)),
    };
    let f = cx.fn_info(def).clone();
    let env = TyEnv::new(f.module, &f.generics.names);
    // An async body returns the promise's payload, which is also the HIR `ret` (the signature,
    // `FnInfo::ret`, and the type of a call stay `Promise<T>`); `None` while it is inferred.
    let body_ret = declared.map(|r| if f.is_async { cx.ty.async_result(r) } else { r });
    let mut frame = Frame::new(f.kind, body_ret);
    frame.is_async = f.is_async;
    let enclosing_locals = cx
        .nested_locals
        .get(&def)
        .or_else(|| f.owner.and_then(|o| cx.nested_locals.get(&o)))
        .cloned()
        .unwrap_or_default();
    let body = match src {
        FnSource::Decl(d) => &d.body,
        FnSource::Default(_, b) => b,
    };
    frame.scopes[0].hi = body.span.hi;
    let mut fcx = FnCx::new(cx, f.module, env, frame);
    fcx.bounds = f.generics.bounds.clone();
    fcx.fn_name = f.name.clone();
    fcx.owner = f.owner;
    fcx.enclosing_locals = enclosing_locals;
    let params = fcx.declare_params(&f);
    let mut stmts = vec![];
    if let (FnKind::Ctor, FnSource::Decl(d)) = (f.kind, src) {
        fcx.ctor_begin(&f, d);
    }
    fcx.stmts_into(&body.stmts, &mut stmts);
    let mut block = hir::Block {
        stmts,
        value: None,
        span: body.span,
    };
    if f.kind == FnKind::Ctor {
        fcx.check_ctor(&f, &block);
    }
    let body_ret = match body_ret {
        Some(r) => r,
        None => fcx.inferred_fn_ret(def, &mut block),
    };
    fcx.check_returns(&f.name, body_ret, f.name_span, &block);
    fcx.rec_frame_scopes();
    let frame = std::mem::replace(&mut fcx.f, Frame::new(f.kind, None));
    let info = cx.fn_info_mut(def);
    info.local_kinds = frame.kinds;
    info.throw_srcs = frame.uncaught;
    info.soft_moves = frame.soft_moves;
    hir::FnDef {
        name: f.name.clone(),
        generics: f.generics.len() as u32,
        params,
        ret: body_ret,
        is_async: f.is_async,
        self_ty: f.this.as_ref().map(|t| t.ty),
        captures: vec![],
        body: hir::Body {
            locals: frame.locals,
            block,
        },
        throws: None,
        span: f.span,
    }
}

impl FnCx<'_, '_> {
    /// `this` and the declared params as the first locals of the body.
    fn declare_params(&mut self, f: &crate::defs::FnInfo) -> Vec<hir::Param> {
        let mut params = vec![];
        if let Some(t) = &f.this {
            let l = self.new_local("this", t.ty, false, f.name_span, LocalKind::This);
            self.f.scopes[0].names.insert("this".into(), l);
            params.push(hir::Param {
                local: l,
                ty: t.ty,
                mode: t.mode,
            });
        }
        for p in &f.params {
            // `mutable` is set when the body assigns or modifies the param (`mark_mutated`).
            let l = self.new_local(&p.name, p.ty, false, p.span, LocalKind::Param);
            self.f.scopes[0].names.entry(p.name.clone()).or_insert(l);
            self.rec_local_decl(l);
            params.push(hir::Param {
                local: l,
                ty: p.ty,
                mode: p.mode,
            });
        }
        params
    }

    /// The result of function `def` inferred from its checked body `block`, recorded as its
    /// signature (`Promise<T>` for an async function).
    fn inferred_fn_ret(&mut self, def: DefId, block: &mut hir::Block) -> hir::TyId {
        let short = self.fn_name.rsplit("::").next().unwrap_or(&self.fn_name);
        let who = format!("`{short}`");
        let (ret, inferred_int) = self.finish_inferred_ret(block, &who);
        let sig = if self.f.is_async {
            self.cx.ty.promise(ret)
        } else {
            ret
        };
        let info = self.cx.fn_info_mut(def);
        info.ret = sig;
        info.ret_source = RetSource::Known;
        info.ret_inferred_int = inferred_int;
        ret
    }

    /// "must return a value on every path" for non-void functions.
    pub fn check_returns(&mut self, name: &str, ret: hir::TyId, span: Span, block: &hir::Block) {
        if ret != self.cx.ty.unit
            && !self.cx.ty.is_bottom(ret)
            && !crate::flow::block_diverges(block, &self.cx.ty)
        {
            let rty = self.cx.display(ret);
            let short = name.rsplit("::").next().unwrap_or(name);
            self.cx.error(
                Diagnostic::error(
                    format!("function `{short}` must return a value of type `{rty}` on every path"),
                    span,
                )
                .with_note(format!("expected {rty}, found void")),
            );
        }
    }
}
