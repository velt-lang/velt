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

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::closure_sig::Expected;
use crate::body::{FnCx, Frame, LocalKind, Want};
use crate::collect::fn_placeholder;
use crate::defs::{BodyState, DefInfo, FnKind, ParamSig};
use crate::hir::{self, Def, DefId, ExprKind as H, LocalId, PassMode, StmtKind as S, TyId};

/// A checked closure, before its locals are renumbered (captures first).
struct Checked {
    def: DefId,
    frame: Frame,
    block: hir::Block,
    declared: Vec<LocalId>,
    ptys: Vec<TyId>,
    ret: TyId,
    captures: Vec<hir::Capture>,
    is_async: bool,
    span: Span,
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
                    "a generic arrow function must be a module-level constant with typed parameters and a return type",
                    e.span,
                )
                .with_note("a function value has one type; write `const id = <T>(x: T): T => x;` at module level, or a generic `function`"),
            );
            return self.error_expr(e.span);
        }
        let (ret, is_async, span) = (ret.as_ref(), *is_async, e.span);
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
        // A future owns everything it uses: async closures always capture by value.
        frame.escaping = escaping || is_async;
        frame.is_async = is_async;
        let saved = std::mem::replace(&mut self.f, frame);
        self.outer.push(saved);
        let mut declared = vec![];
        for (p, ty) in params.iter().zip(&ptys) {
            let l = self.declare_local_mut(&p.name, *ty, LocalKind::Param, false);
            if std_callback && p.ty.is_none() && self.cx.ty.is_int(*ty) {
                self.f.inferred_ints.insert(l);
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
        let block = self.closure_body(body, span);
        self.rec_frame_scopes();
        let body_ret = self.f.ret.unwrap_or(self.cx.ty.unit);
        let parent = self.outer.pop().expect("ICE: closure frame");
        let frame = std::mem::replace(&mut self.f, parent);
        if is_async {
            self.no_mutated_captures(&frame);
        }
        let captures = self.capture_modes(&frame, span);
        let clause = throws.as_ref().map_or(span, |t| t.span);
        let err = self.closure_error(def, declared_err, &frame, clause);
        let fn_ty = self.closure_type(ptys.clone(), body_ret, err, is_async);
        let ret = if is_async {
            self.cx.ty.promise(body_ret)
        } else {
            body_ret
        };
        self.cx.fn_info_mut(def).escaping = escaping || is_async;
        self.finish_closure(Checked {
            def,
            frame,
            block,
            declared,
            ptys,
            ret,
            captures,
            is_async,
            span,
        });
        self.mk(H::Closure(def), fn_ty, span)
    }

    /// Async closures run as tasks, possibly on another thread and after the enclosing function
    /// has moved on: mutating a captured variable would be a data race (or lost), so it is an
    /// error; shared state goes through `shared` (docs/reference/async.md).
    fn no_mutated_captures(&mut self, frame: &Frame) {
        for c in &frame.captures {
            let Some(at) = c.mutated_at else { continue };
            let name = frame.locals[c.inner.0 as usize].name.clone();
            self.cx.error(
                Diagnostic::error(
                    format!("cannot mutate captured variable `{name}` in a spawned task (async closure)"),
                    at,
                )
                .with_note(format!(
                    "tasks may run concurrently on other threads; share it with `shared` instead: `const {name} = shared(...)` and `{name}.add(n)` / `{name}.set(v)`, or `shared(new Mutex(...))` with `.with(...)`"
                )),
            );
        }
    }

    fn alloc_closure(&mut self, span: Span) -> DefId {
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
        self.cx.alloc_def(span, DefInfo::Fn(Box::new(info)))
    }

    fn closure_body(&mut self, body: &ast::ArrowBody, span: Span) -> hir::Block {
        match body {
            ast::ArrowBody::Expr(e) => {
                let h = match self.f.ret {
                    Some(r) => self.expr_coerce(e, r, Want::Move),
                    None => self.expr(e, None, Want::Move),
                };
                if self.f.ret.is_none() {
                    self.f.ret = Some(h.ty);
                }
                let hs = h.span;
                let kind = if h.ty == self.cx.ty.unit {
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
                let block = hir::Block {
                    stmts,
                    value: None,
                    span: b.span,
                };
                let ret = self.f.ret.unwrap_or(self.cx.ty.unit);
                self.check_returns("closure", ret, span, &block);
                block
            }
        }
    }

    /// Final capture modes; the enclosing function must allow what the closure does.
    fn capture_modes(&mut self, frame: &Frame, span: Span) -> Vec<hir::Capture> {
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

    fn finish_closure(&mut self, c: Checked) {
        let Checked {
            def,
            frame,
            mut block,
            declared,
            ptys,
            ret,
            mut captures,
            is_async,
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
        let sigs =
            self.declared_params(&frame, &declared, &ptys, &map, &mut params, is_async, span);
        let info = self.cx.fn_info_mut(def);
        info.params = sigs;
        info.ret = ret;
        info.is_async = is_async;
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
            is_async,
            self_ty: None,
            captures,
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
