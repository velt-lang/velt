//! Checking order: field defaults, module constants, parameter defaults, then every function
//! body. Bodies are checked on demand (`ensure_body`) because a `catch (e)` needs the thrown
//! types of the functions called in its `try`, which are only known once their bodies are.

use velt_common::{Diagnostic, Span};

use super::{FnCx, Frame, LocalKind, Want};
use crate::ctx::Ctx;
use crate::defs::{BodyState, DefInfo, FnKind, FnSource};
use crate::hir::{self, Def, DefId, ExprKind as H, LocalId, StmtKind as S};
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

/// Check `def`'s body now unless it is already checked or being checked.
pub(crate) fn ensure_body(cx: &mut Ctx, def: DefId) {
    let f = cx.fn_info(def);
    if f.state != BodyState::Unchecked {
        return;
    }
    let Some(src) = f.source else {
        cx.fn_info_mut(def).state = BodyState::Done;
        return;
    };
    cx.fn_info_mut(def).state = BodyState::InProgress;
    let names = cx.fn_info(def).generics.names.clone();
    let saved = std::mem::replace(&mut cx.display_params, names);
    let fndef = check_fn(cx, def, src);
    cx.display_params = saved;
    cx.defs[def.0 as usize] = Some(Def::Fn(fndef));
    cx.fn_info_mut(def).state = BodyState::Done;
}

fn check_fn(cx: &mut Ctx, def: DefId, src: FnSource) -> hir::FnDef {
    let f = cx.fn_info(def).clone();
    let env = TyEnv::new(f.module, &f.generics.names);
    // An async body returns the promise's payload, which is also the HIR `ret` (the signature,
    // `FnInfo::ret`, and the type of a call stay `Promise<T>`).
    let body_ret = if f.is_async {
        cx.ty.async_result(f.ret)
    } else {
        f.ret
    };
    // A generator's body yields `T` of its declared `Generator<T>` and returns nothing; the HIR
    // `ret` stays the declared result (hir_encodings.md "Generators").
    let yield_ty = f.is_generator.then(|| {
        cx.generator_result(f.ret)
            .and_then(|(_, a)| a.first().copied())
            .unwrap_or(cx.ty.error)
    });
    let frame_ret = if yield_ty.is_some() {
        cx.ty.unit
    } else {
        body_ret
    };
    let mut frame = Frame::new(f.kind, Some(frame_ret));
    frame.is_async = f.is_async;
    frame.yield_ty = yield_ty;
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
    if f.kind == FnKind::Ctor && fcx.this_base().is_some() {
        // Until `super(...)`, which a base class with a constructor requires.
        fcx.f.before_super =
            fcx.base_ctor(&f).is_some() || body.stmts.iter().any(super::stmt::is_super_call);
    }
    fcx.stmts_into(&body.stmts, &mut stmts);
    let block = hir::Block {
        stmts,
        value: None,
        span: body.span,
    };
    if f.kind == FnKind::Ctor {
        fcx.check_ctor(&f, &block);
    }
    fcx.check_returns(&f.name, frame_ret, f.name_span, &block);
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
        is_generator: f.is_generator,
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

    /// The constructor of the base class of constructor `f`'s class, if any.
    fn base_ctor(&mut self, f: &crate::defs::FnInfo) -> Option<DefId> {
        let a = self.cx.adt(f.owner?)?;
        let (b, _) = self.cx.class_of(a.base?)?;
        self.cx.adt(b).and_then(|x| x.ctor)
    }

    /// Constructor rules: `super(...)` first when the base class has a constructor, and every
    /// own field without a default assigned on every path. Records what the field initializers
    /// the constructor runs on entry may throw.
    fn check_ctor(&mut self, f: &crate::defs::FnInfo, block: &hir::Block) {
        let Some(owner) = f.owner else { return };
        let base_ctor = self.base_ctor(f);
        let a = self.cx.adt(owner).expect("ICE: ctor owner");
        let needed: Vec<(u32, String)> = a.fields[a.own_fields_start..]
            .iter()
            .enumerate()
            .filter(|(_, fl)| !fl.has_default)
            .map(|(i, fl)| ((a.own_fields_start + i) as u32, fl.name.clone()))
            .collect();
        let class = a.name.clone();
        if base_ctor.is_none() {
            // No ancestor has a constructor: this one runs every field initializer on entry
            // (with one, they run after `super(...)`, which accounts for them).
            let this_ty = self.this_ty();
            for s in self.class_default_throws(this_ty, None, f.name_span) {
                self.throw_src(s);
            }
        }
        if base_ctor.is_some() && !self.f.super_called {
            self.cx.error(
                Diagnostic::error(
                    format!("the constructor of `{class}` must call `super(...)`"),
                    f.name_span,
                )
                .with_note("the base class has a constructor that must run"),
            );
        }
        let this = LocalId(0);
        let assigned = assigned_fields(block, this);
        for (idx, name) in needed {
            if !assigned.contains(&idx) {
                self.cx.error(
                    Diagnostic::error(
                        format!(
                            "field `{name}` is not initialized by the constructor of `{class}`"
                        ),
                        f.name_span,
                    )
                    .with_note(format!("assign `this.{name} = ...` on every path")),
                );
            }
        }
    }
}

/// Fields of `this` assigned on every path through `b` (conservative).
fn assigned_fields(b: &hir::Block, this: LocalId) -> Vec<u32> {
    let mut out = vec![];
    for s in &b.stmts {
        match &s.kind {
            S::Expr(e) => field_assign(e, this, &mut out),
            S::Block(inner) => out.extend(assigned_fields(inner, this)),
            S::If {
                then,
                els: Some(els),
                ..
            } => {
                let (t, e) = (assigned_fields(then, this), assigned_fields(els, this));
                out.extend(t.into_iter().filter(|x| e.contains(x)));
            }
            _ => {}
        }
    }
    out
}

fn field_assign(e: &hir::Expr, this: LocalId, out: &mut Vec<u32>) {
    if let H::Assign { place, .. } = &e.kind {
        if let H::Field { base, index, .. } = &place.kind {
            if matches!(base.kind, H::Local(l, _) if l == this) {
                out.push(*index);
            }
        }
    }
}
