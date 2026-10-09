//! Arrow functions. Each closure is its own `Def::Fn` (named `<enclosing fn>::{closure#N}`,
//! sharing the enclosing function's generics) checked in a nested frame; names of enclosing
//! locals become captures.
//!
//! Capture modes (docs/reference/functions.md "Captures"):
//! - non-escaping (direct call argument to a function-typed parameter, or immediately called):
//!   mutated → `BorrowMut`, Copy → `Copy`, otherwise `Borrow`;
//! - escaping (stored / returned): Copy and never mutated → `Copy`, otherwise `Owned` (the
//!   captured variable is moved into the closure).
//!
//! Encoding: `FnDef::params` = one param per capture (local = `Capture::inner`, mode = capture
//! mode) followed by the declared params; body locals are numbered the same way (captures first).
//! The closure value's type is `FnPtr { declared params, ret, throws }` (its error type is the
//! one of the function type expected where it is created, else what its body throws).
//! Declared params: Copy ones are copied, other ones borrowed (`Borrow`) — also when the body
//! modifies them: a closure may be called with aliasing arguments, so its params never get
//! `BorrowMut`'s no-alias guarantee
//! (`LocalDef::mutable` records the modification instead; see `crate::ownership`).
//! Capture modes that depend on inferred mutation are finalized by `crate::ownership`.

use velt_common::Span;
use velt_syntax::ast;

use super::closure_sig::Expected;
use crate::body::{FnCx, Frame, LocalKind, Want};
use crate::collect::fn_placeholder;
use crate::defs::{BodyState, DefInfo, FnKind, ParamSig};
use crate::hir::{self, Def, DefId, ExprKind as H, LocalId, PassMode, StmtKind as S, TyId};

/// A checked closure, before its locals are renumbered (captures first).
pub(super) struct Checked {
    pub def: DefId,
    pub frame: Frame,
    pub block: hir::Block,
    pub declared: Vec<LocalId>,
    pub ptys: Vec<TyId>,
    pub ret: TyId,
    pub captures: Vec<hir::Capture>,
    pub is_async: bool,
    /// A generator function expression (`gen_closure.rs`): `Some(is_async)`.
    pub generator: Option<bool>,
    pub span: Span,
}

/// New local order: captures, then declared params, then the rest.
fn local_order(frame: &Frame, captures: &[hir::Capture], declared: &[LocalId]) -> Vec<LocalId> {
    let mut order: Vec<LocalId> = captures.iter().map(|c| c.inner).collect();
    order.extend(declared.iter().copied());
    for i in 0..frame.locals.len() as u32 {
        if !order.contains(&LocalId(i)) {
            order.push(LocalId(i));
        }
    }
    order
}

impl FnCx<'_, '_> {
    /// The first of `members` that arrow `e` type-checks against (each try is rolled back), or
    /// the first one, whose errors the real check then reports.
    fn member_by_trial(&mut self, e: &ast::Expr, members: &[TyId], escaping: bool) -> TyId {
        for &m in members {
            let mark = crate::body::recheck::Mark::here(self.cx);
            let frames = (
                self.f.clone(),
                self.outer.clone(),
                self.refused_reads.clone(),
                self.literal.clone(),
            );
            let diags = self.cx.diags.len();
            let h = self.closure(e, Some(m), escaping);
            let ok = self.cx.diags.len() == diags && h.ty == m;
            mark.rollback(self.cx);
            (self.f, self.outer, self.refused_reads, self.literal) = frames;
            if ok {
                return m;
            }
        }
        members[0]
    }

    /// An arrow function (`e` is an `ast::ExprKind::Arrow`) where a value of type `exp` is
    /// expected; `escaping` when it is stored or returned rather than passed to a call.
    pub(crate) fn closure(
        &mut self,
        e: &ast::Expr,
        exp: Option<TyId>,
        escaping: bool,
    ) -> hir::Expr {
        let ast::ExprKind::Arrow {
            type_params,
            params,
            ret,
            throws,
            body,
            is_async,
        } = &e.kind
        else {
            unreachable!("ICE: closure of a non-arrow expression")
        };
        if !type_params.is_empty() {
            self.cx.error(
                velt_common::Diagnostic::error(
                    "a generic arrow function must be the value of a `const` with typed parameters",
                    e.span,
                )
                .with_note("a function value has one type; declare it as `const id = <T>(x: T) => x;` and call it, or write a generic `function`"),
            );
            return self.error_expr(e.span);
        }
        // Against a union of function types, the arrow is typed by one member; the caller
        // converts the closure to the union.
        let members = self.arrow_members(exp, *is_async, params.len());
        let exp = match members.as_slice() {
            [] => exp,
            [one] => Some(*one),
            several => Some(self.member_by_trial(e, several, escaping)),
        };
        if *is_async {
            if let Some(h) = self.async_arrow_callback(e, exp) {
                return h;
            }
        }
        let void_task = self.void_task == Some(e.span) && self.is_void_task(params, ret, body);
        if void_task {
            self.void_task = None;
        }
        let (ret, is_async, span) = (ret.as_ref(), *is_async || void_task, e.span);
        let Expected {
            params: exp_params,
            ret: exp_ret,
            throws: exp_throws,
        } = self.expected_fn(exp, is_async);
        let declared_err = match throws {
            Some(t) => Some(self.resolve(t)),
            None => exp_throws,
        };
        let mut ptys = self.closure_param_types(params, exp_params.as_deref());
        // Like TS, an arrow may take fewer parameters than the function type it is passed as
        // (`xs.map((x) => …)` where `map` passes `(x, i)`): the rest are unnamed and unused.
        let extra: Vec<TyId> = exp_params
            .as_deref()
            .filter(|ps| ps.len() > params.len() && ptys.len() == params.len())
            .map_or(vec![], |ps| ps[params.len()..].to_vec());
        ptys.extend(&extra);
        let std_callback = std::mem::take(&mut self.std_callback);
        // Trailing parameters with defaults beyond the expected function type's
        // (`apply((x, k = 2) => x * k)`): never passed, so they are locals set to the default.
        let n_params = exp_params
            .as_deref()
            .map(|ps| ps.len())
            .filter(|&n| n < params.len() && params[n..].iter().all(|p| p.default.is_some()))
            .unwrap_or(params.len());
        let ret_ty = match ret {
            Some(t) => Some(self.closure_ret_annotation(t, is_async)),
            None => exp_ret
                .map(|r| {
                    if is_async {
                        self.cx.ty.async_result(r)
                    } else {
                        r
                    }
                })
                .filter(|r| !self.cx.ty.has_error(*r)),
        };
        let def = self.alloc_closure(span);
        let mut frame = Frame::new(FnKind::Closure, ret_ty);
        frame.scopes[0].hi = span.hi;
        frame.closure_assigned = closure_assigned_in(params, body);
        // A future owns everything it uses: async closures always capture by value.
        frame.escaping = escaping || is_async;
        frame.discards_value = ret.is_none() && !is_async && ret_ty == Some(self.cx.ty.unit);
        frame.is_async = is_async;
        // A JS API callback returning a number (a comparator) may return any integer.
        frame.int_returns_number = std_callback.is_some() && ret_ty == Some(self.cx.ty.f64);
        let saved = std::mem::replace(&mut self.f, frame);
        self.outer.push(saved);
        let mut declared = vec![];
        let mut locals = vec![];
        for (k, (p, ty)) in params.iter().zip(&ptys).enumerate() {
            if k >= n_params {
                locals.push(self.defaulted_local(p, *ty));
                continue;
            }
            let l = self.declare_local_mut(&p.name, *ty, LocalKind::Param, false);
            // An index from a std callback is a number in the body.
            let std_int = std_callback
                .as_ref()
                .and_then(|m| m.get(k).copied())
                .unwrap_or(false);
            if std_int && p.ty.is_none() && self.cx.ty.is_int(*ty) {
                locals.push(self.number_shadow(l));
            }
            declared.push(l);
        }
        for (k, ty) in extra.iter().enumerate() {
            // Not a valid identifier, so the body cannot name it.
            let name = ast::Ident {
                name: format!("#unused{k}"),
                span: Span::new(span.file, span.lo, span.lo),
            };
            let l = self.declare_local_mut(&name, *ty, LocalKind::Param, false);
            declared.push(l);
        }
        let mut block = match body {
            ast::ArrowBody::Expr(x) if void_task => self.void_task_body(x),
            _ => self.closure_body(body, span),
        };
        block.stmts.splice(0..0, locals);
        let ptys: Vec<TyId> = ptys
            .iter()
            .enumerate()
            .filter(|(k, _)| *k < n_params || *k >= params.len())
            .map(|(_, t)| *t)
            .collect();
        self.rec_frame_scopes();
        let body_ret = self.f.ret.unwrap_or(self.cx.ty.unit);
        let parent = self.outer.pop().expect("ICE: closure frame");
        self.finish_using_shares();
        let frame = std::mem::replace(&mut self.f, parent);
        let mutated = if is_async {
            self.no_captured_generators(&frame);
            mutated_captures(&frame)
        } else {
            vec![]
        };
        let captures = self.capture_modes(&frame, span);
        let clause = throws.as_ref().map_or(span, |t| t.span);
        let err = self.closure_error(def, declared_err, &frame, clause);
        let fn_ty = self.closure_type(ptys.clone(), body_ret, err, is_async);
        let ret = if is_async {
            self.cx.ty.promise(body_ret)
        } else {
            body_ret
        };
        let info = self.cx.fn_info_mut(def);
        info.escaping = escaping || is_async;
        info.mutated_captures = mutated;
        self.finish_closure(Checked {
            def,
            frame,
            block,
            declared,
            ptys,
            ret,
            captures,
            is_async,
            generator: None,
            span,
        });
        crate::body::defaults::arrow_defaults(self.cx, self.module, def, params);
        self.mk(H::Closure(def), fn_ty, span)
    }

    /// `let p = <default>;` for an arrow parameter that is never passed (see `closure`).
    fn defaulted_local(&mut self, p: &ast::ArrowParam, ty: TyId) -> hir::Stmt {
        let e = p.default.as_ref().expect("ICE: a defaulted parameter");
        let init = self.expr_coerce(e, ty, Want::Move);
        let local = self.declare_local_mut(&p.name, ty, LocalKind::Let, true);
        hir::Stmt {
            kind: S::Let {
                local,
                init: Some(init),
            },
            span: p.name.span,
        }
    }

    /// An async closure copies its captures when it runs: none may hold a generator.
    pub(super) fn no_captured_generators(&mut self, frame: &Frame) {
        for c in &frame.captures {
            let l = &frame.locals[c.inner.0 as usize];
            let (ty, name, at) = (l.ty, l.name.clone(), l.span);
            self.no_generator_copy(ty, crate::body::GenCopy::Capture(name), at);
        }
    }

    pub(super) fn alloc_closure(&mut self, span: Span) -> DefId {
        let n = self
            .cx
            .closure_counts
            .entry(self.fn_name.clone())
            .or_insert(0);
        let name = format!("{}::{{closure#{}}}", self.fn_name, n);
        *n += 1;
        let mut info = fn_placeholder(name, span, span, self.module, FnKind::Closure, None);
        info.generics.names = self.env.params.clone();
        info.generics.bounds = self.bounds.clone();
        info.generics
            .bounds
            .resize(info.generics.names.len(), vec![]);
        info.state = BodyState::Done;
        let info = DefInfo::Fn(Box::new(info));
        // A check after a rollback creates the same closures again (`recheck`).
        let def = match crate::body::recheck::reuse_closure(self.cx) {
            Some(d) => {
                self.cx.redefine_closure(d, span, info);
                d
            }
            None => self.cx.alloc_def(span, info),
        };
        self.cx.closure_defs.push(def);
        def
    }

    fn closure_body(&mut self, body: &ast::ArrowBody, span: Span) -> hir::Block {
        match body {
            ast::ArrowBody::Expr(e) => {
                let h = match self.f.ret {
                    Some(_) if self.f.discards_value => self.expr_stmt(e),
                    Some(r) => self.returned(e, r),
                    None => self.expr(e, None, Want::Move),
                };
                if self.f.ret.is_none() {
                    self.f.ret = Some(h.ty);
                }
                let hs = h.span;
                let kind = if h.ty == self.cx.ty.unit || self.f.discards_value {
                    S::Expr(h)
                } else {
                    S::Return(Some(h))
                };
                hir::Block {
                    stmts: vec![hir::Stmt { kind, span: hs }],
                    value: None,
                    span: e.span,
                }
            }
            ast::ArrowBody::Block(b) => {
                let mut stmts = vec![];
                self.stmts_into(&b.stmts, &mut stmts);
                let mut block = hir::Block {
                    stmts,
                    value: None,
                    span: b.span,
                };
                let ret = match self.f.ret {
                    Some(r) => r,
                    None => self.finish_inferred_ret(&mut block, "this arrow function"),
                };
                self.check_returns("closure", ret, span, &block);
                block
            }
        }
    }

    /// Final capture modes; the enclosing function must allow what the closure does.
    pub(super) fn capture_modes(&mut self, frame: &Frame, span: Span) -> Vec<hir::Capture> {
        let escaping = frame.escaping;
        let mut out = vec![];
        for c in &frame.captures {
            let ty = frame.locals[c.inner.0 as usize].ty;
            let copy = self.cx.is_copy(ty);
            let mode = match (escaping, c.mutated, copy) {
                (false, true, _) => PassMode::BorrowMut,
                (false, false, true) => PassMode::Copy,
                (false, false, false) => PassMode::Borrow,
                (true, false, true) => PassMode::Copy,
                (true, _, _) => PassMode::Owned,
            };
            if mode == PassMode::BorrowMut {
                let place = self.mk(H::Local(c.outer, hir::UseMode::BorrowMut), ty, span);
                self.require_capture_mut(&place, c.outer);
            }
            out.push(hir::Capture {
                outer: c.outer,
                inner: c.inner,
                mode,
                share: false,
            });
        }
        out
    }

    /// A non-escaping closure mutates enclosing variable `outer`.
    fn require_capture_mut(&mut self, place: &hir::Expr, outer: LocalId) {
        if self.local_kind(outer) == LocalKind::Const && self.cx.is_copy(place.ty) {
            let name = self.f.locals[outer.0 as usize].name.clone();
            self.cx
                .err(format!("cannot assign twice to const `{name}`"), place.span);
            return;
        }
        self.require_mutable(place, "mutate");
    }

    pub(super) fn finish_closure(&mut self, c: Checked) {
        let Checked {
            def,
            frame,
            mut block,
            declared,
            ptys,
            ret,
            mut captures,
            is_async,
            generator,
            span,
        } = c;
        let order = local_order(&frame, &captures, &declared);
        let mut map = vec![LocalId(0); order.len()];
        for (new, old) in order.iter().enumerate() {
            map[old.0 as usize] = LocalId(new as u32);
        }
        crate::visit::remap_locals(&mut block, &map);
        self.remap_nested_captures(&mut block, &map);
        let mut locals = vec![];
        let mut kinds = vec![];
        for old in &order {
            let mut l = frame.locals[old.0 as usize].clone();
            if let Some(c) = frame.captures.iter().find(|c| c.inner == *old) {
                l.mutable = c.mutated;
            }
            locals.push(l);
            kinds.push(frame.kinds[old.0 as usize]);
        }
        for c in &mut captures {
            c.inner = map[c.inner.0 as usize];
        }
        let mut params: Vec<hir::Param> = captures
            .iter()
            .map(|c| hir::Param {
                local: c.inner,
                ty: locals[c.inner.0 as usize].ty,
                mode: c.mode,
            })
            .collect();
        // A generator keeps its arguments until it is done: they are owned, like an async one's.
        let owned = is_async || generator.is_some();
        let sigs = self.declared_params(&frame, &declared, &ptys, &map, &mut params, owned, span);
        let info = self.cx.fn_info_mut(def);
        info.params = sigs;
        info.ret = ret;
        info.is_async = is_async;
        info.is_generator = generator.is_some();
        info.is_async_gen = generator == Some(true);
        info.local_kinds = kinds;
        info.throw_srcs = frame.uncaught;
        info.soft_moves = frame.soft_moves;
        // The HIR `ret` of an async function is what its body returns (`T` of `Promise<T>`).
        let body_ret = if is_async {
            self.cx.ty.async_result(ret)
        } else {
            ret
        };
        let info = self.cx.fn_info(def);
        let fndef = hir::FnDef {
            name: info.name.clone(),
            generics: info.generics.len() as u32,
            params,
            ret: body_ret,
            is_async: is_async || generator == Some(true),
            is_generator: generator.is_some(),
            self_ty: None,
            captures,
            shares_captures: false,
            body: hir::Body { locals, block },
            throws: None,
            span,
        };
        self.cx.defs[def.0 as usize] = Some(Def::Fn(fndef));
    }

    /// The declared params (after the capture params): non-Copy ones borrowed, or owned by an
    /// async closure (its code clones them into the future; see hir.rs "M3 additions").
    #[allow(clippy::too_many_arguments)] // the closure's frame, local renumbering and output
    fn declared_params(
        &mut self,
        frame: &Frame,
        declared: &[LocalId],
        ptys: &[TyId],
        map: &[LocalId],
        params: &mut Vec<hir::Param>,
        is_async: bool,
        span: Span,
    ) -> Vec<ParamSig> {
        let mut sigs = vec![];
        for (l, ty) in declared.iter().zip(ptys) {
            let mode = if self.cx.is_copy(*ty) {
                PassMode::Copy
            } else if is_async {
                PassMode::Owned
            } else {
                PassMode::Borrow
            };
            params.push(hir::Param {
                local: map[l.0 as usize],
                ty: *ty,
                mode,
            });
            sigs.push(ParamSig {
                name: frame.locals[l.0 as usize].name.clone(),
                span,
                ty: *ty,
                mode,
                default: None,
            });
        }
        sigs
    }

    /// Closures nested in this one refer to its locals in their `Capture::outer`.
    fn remap_nested_captures(&mut self, block: &mut hir::Block, map: &[LocalId]) {
        let mut nested = vec![];
        crate::visit::exprs_mut(block, &mut |e: &mut hir::Expr| {
            if let H::Closure(d) = e.kind {
                nested.push(d);
            }
        });
        for d in nested {
            if let Some(Def::Fn(f)) = &mut self.cx.defs[d.0 as usize] {
                for c in &mut f.captures {
                    c.outer = map[c.outer.0 as usize];
                }
            }
        }
    }
}

/// The variables that closures created in an arrow function (`params`, `body`) assign.
fn closure_assigned_in(
    params: &[ast::ArrowParam],
    body: &ast::ArrowBody,
) -> std::collections::HashMap<String, Span> {
    let defaults = params.iter().filter_map(|p| p.default.as_ref());
    let assigned = match body {
        ast::ArrowBody::Block(b) => crate::body::assigned::assigned_by_closures(&b.stmts, defaults),
        ast::ArrowBody::Expr(e) => {
            crate::body::assigned::assigned_by_closures(&[], defaults.chain([&**e]))
        }
    };
    crate::body::closure_assigned::owned(assigned)
}

/// The captured variables an async closure's body modifies, with the first place each is
/// modified. They are only allowed when the closure stays on its task
/// (`crate::ownership::local_async`).
fn mutated_captures(frame: &Frame) -> Vec<(String, Span)> {
    frame
        .captures
        .iter()
        .filter_map(|c| {
            let at = c.mutated_at?;
            Some((frame.locals[c.inner.0 as usize].name.clone(), at))
        })
        .collect()
}
