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
use crate::body::stmt::void_op;
use crate::body::{FnCx, Frame, LocalKind, Want};
use crate::collect::fn_placeholder;
use crate::defs::{BodyState, DefInfo, FnKind, ParamSig};
use crate::hir::{self, Def, DefId, ExprKind as H, LocalId, PassMode, StmtKind as S, TyId};

/// What [`FnCx::trial_frames`] saves.
pub(crate) struct TrialFrames {
    f: Frame,
    outer: Vec<Frame>,
    refused_reads: Vec<(Span, crate::body::closure_assigned::Refused)>,
    literal: crate::body::literal_locals::LiteralLocals,
    std_callback: Option<Vec<bool>>,
    void_task: Option<Span>,
    thread_task: Option<Span>,
    await_body: Option<Span>,
}

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
    /// The function's state a trial check changes, to restore after rolling it back (`Mark`
    /// restores the context): locals, captures, throws, moves, literal-local uses and the
    /// pending callback notes of the call being checked.
    pub(crate) fn trial_frames(&self) -> TrialFrames {
        TrialFrames {
            f: self.f.clone(),
            outer: self.outer.clone(),
            refused_reads: self.refused_reads.clone(),
            literal: self.literal.clone(),
            std_callback: self.std_callback.clone(),
            void_task: self.void_task,
            thread_task: self.thread_task,
            await_body: self.await_body,
        }
    }

    pub(crate) fn restore_trial_frames(&mut self, t: TrialFrames) {
        self.f = t.f;
        self.outer = t.outer;
        self.refused_reads = t.refused_reads;
        self.literal = t.literal;
        self.std_callback = t.std_callback;
        self.void_task = t.void_task;
        self.thread_task = t.thread_task;
        self.await_body = t.await_body;
    }

    /// The first of `members` that arrow `e` type-checks against (each try is rolled back), or
    /// the first one, whose errors the real check then reports.
    fn member_by_trial(&mut self, e: &ast::Expr, members: &[TyId], escaping: bool) -> TyId {
        // Remembered per arrow, members and the types of the enclosing locals it names: an
        // arrow nested in one being tried is tried once, not once per try of each enclosing
        // arrow (exponential in the nesting), unless a local it uses has another type in this
        // try (`(x) => { const y = x; show(() => y); }`, `x` a number, then a string). Locals
        // it doesn't name (the enclosing arrows' other locals) don't change how it checks.
        let key = (e.span, members.to_vec(), self.named_local_types(e));
        if let Some(&m) = self.member_choices.get(&key) {
            return m;
        }
        let m = self.first_fitting(e, members, escaping);
        self.member_choices.insert(key, m);
        m
    }

    /// The types of the locals (of this function and of those it is nested in) named like a
    /// value arrow `e` takes from its surroundings, or `this`.
    fn named_local_types(&mut self, e: &ast::Expr) -> Vec<TyId> {
        let free = self.arrow_scans.free_names(e);
        let frames = self.outer.iter().chain(std::iter::once(&self.f));
        frames
            .flat_map(|f| f.locals.iter())
            .filter(|l| free.binary_search(&l.name).is_ok())
            .map(|l| l.ty)
            .collect()
    }

    fn first_fitting(&mut self, e: &ast::Expr, members: &[TyId], escaping: bool) -> TyId {
        for &m in members {
            let mark = crate::body::recheck::Mark::here(self.cx);
            let frames = self.trial_frames();
            let diags = self.cx.diags.len();
            let h = self.closure(e, Some(m), escaping);
            let ok = self.cx.diags.len() == diags && h.ty == m;
            mark.rollback(self.cx);
            self.restore_trial_frames(frames);
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
        if let Some(h) = self.instantiated_arrow(e, exp, escaping) {
            return h;
        }
        if self.is_generic_closure(e, exp) {
            return self.generic_closure(e, exp);
        }
        // Stored into a site, it throws into that one (`throws::fn_flows`); its body stores
        // nothing there.
        self.cx.note_closure_site(e.span, exp);
        let target = self.set_target(None);
        let h = self.closure_in(e, exp, escaping);
        self.set_target(target);
        h
    }

    fn closure_in(&mut self, e: &ast::Expr, exp: Option<TyId>, escaping: bool) -> hir::Expr {
        let ast::ExprKind::Arrow {
            params,
            ret,
            throws,
            body,
            is_async,
            ..
        } = &e.kind
        else {
            unreachable!("ICE: closure of a non-arrow expression")
        };
        crate::check_work::add_closure_check();
        // A member of a recursive closure group (`closure_group`).
        let member = self.pending_member.take();
        // An object-literal method using `this` (`literal_this.rs`).
        if super::literal_this::takes_this(e) {
            let at = super::literal_this::first_this(e);
            if self.method_this_ty(e.span, at).is_none() {
                return self.error_expr(e.span);
            }
        }
        // A rest parameter's type is checked as a function's is.
        if let Some(t) = params.last().filter(|p| p.rest).and_then(|p| p.ty.as_ref()) {
            let ty = self.resolve(t);
            if !crate::collect::sigs::rest_param_type(self.cx, t.span, ty) {
                return self.error_expr(e.span);
            }
        }
        // Against a union of function types, the arrow is typed by one member; the caller
        // converts the closure to the union.
        let members = self.arrow_members(exp, *is_async, params.len());
        let exp = match members.as_slice() {
            [] => exp,
            [one] => Some(*one),
            several => Some(self.member_by_trial(e, several, escaping)),
        };
        if !*is_async && self.thread_task == Some(e.span) {
            self.thread_task = None;
            return self.thread_arrow(e, exp, escaping);
        }
        if *is_async {
            if let Some(h) = self.async_arrow_callback(e, exp) {
                return h;
            }
        } else if let Some(h) = self.declared_member_callback(e, exp) {
            return h;
        }
        if let Some(h) = self.rest_shape_callback(e, exp) {
            return h;
        }
        if let Some(h) = self.frameless_widened_params(e, exp) {
            return h;
        }
        let await_body = self.await_body == Some(e.span) && *is_async;
        if await_body {
            self.await_body = None;
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
            rest: exp_rest,
        } = self.expected_fn(exp, is_async);
        let rest = match self.packed_rest == Some(span) {
            true => {
                self.packed_rest = None;
                Some(false)
            }
            false => self.closure_rest(params, exp_params.as_deref(), exp_rest),
        };
        let Some(rest) = rest else {
            return self.error_expr(span);
        };
        let declared_err = match throws {
            Some(t) => Some(self.resolve(t)),
            None => exp_throws,
        };
        let untyped = (self.any_callback == Some(span))
            .then(|| self.any_ty())
            .flatten();
        let mut ptys = self.closure_param_types(params, exp_params.as_deref(), untyped);
        // Like TS, an arrow may take fewer parameters than the function type it is passed as
        // (`xs.map((x) => …)` where `map` passes `(x, i)`): the rest are unnamed and unused.
        let extra: Vec<TyId> = exp_params
            .as_deref()
            .filter(|ps| ps.len() > params.len() && ptys.len() == params.len())
            .map_or(vec![], |ps| ps[params.len()..].to_vec());
        ptys.extend(&extra);
        let std_callback = std::mem::take(&mut self.std_callback);
        let promise_callback = std::mem::take(&mut self.promise_callback);
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
        let def = match member {
            Some((_, _, d)) => d,
            None => self.alloc_closure(span),
        };
        let mut frame = Frame::new(FnKind::Closure, ret_ty);
        frame.scopes[0].hi = span.hi;
        frame.closure_assigned = self.arrow_scans.closure_assigned(e);
        frame.assign_ends = self.arrow_scans.assignment_ends(e);
        // A future owns everything it uses: async closures always capture by value.
        frame.escaping = escaping || is_async;
        frame.discards_value = ret.is_none() && !is_async && ret_ty == Some(self.cx.ty.unit);
        frame.asserted_ret = ret.is_none() && self.asserted_fn == Some(e.span);
        frame.never_end = ret.is_none() && (exp.is_none() || promise_callback);
        frame.is_async = is_async;
        // A JS API callback returning a number (a comparator) may return any integer.
        frame.int_returns_number = std_callback.is_some() && ret_ty == Some(self.cx.ty.f64);
        // A JS API predicate's result is tested like a condition (`xs.filter((n) => n)`).
        frame.truthy_returns =
            std_callback.is_some() && ret.is_none() && ret_ty == Some(self.cx.ty.bool_);
        let saved = std::mem::replace(&mut self.f, frame);
        self.outer.push(saved);
        if let Some((g, m, _)) = member {
            self.enter_group_member(g, m, span);
        }
        let mut declared = vec![];
        let mut locals = vec![];
        let std_ints: Vec<bool> = (0..params.len())
            .map(|k| {
                std_callback
                    .as_ref()
                    .is_some_and(|m| m.get(k) == Some(&true))
            })
            .collect();
        let wide = self.entry_widened(params, &ptys, exp_params.as_deref(), &std_ints, n_params);
        for (k, (p, ty)) in params.iter().zip(&ptys).enumerate() {
            if k >= n_params {
                locals.push(self.defaulted_local(p, *ty));
                continue;
            }
            // An index from a std callback is a number in the body.
            let std_int = std_callback
                .as_ref()
                .and_then(|m| m.get(k).copied())
                .unwrap_or(false);
            if let Some(passed) = wide[k] {
                let (l, shadow) = self.widened_param(p, passed, *ty, std_int);
                declared.push(l);
                locals.push(shadow);
                continue;
            }
            let l = self.declare_local_mut(&p.name, *ty, LocalKind::Param, false);
            self.note_readonly_array(l, p.ty.as_ref());
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
        self.default_prologue(
            params
                .iter()
                .zip(&ptys)
                .take(n_params)
                .map(|(p, ty)| (&p.name, p.default.as_ref(), p.optional, *ty)),
            &mut locals,
        );
        self.destructure_params(
            params
                .iter()
                .map(|p| (&p.name, p.pattern.as_ref(), p.optional)),
            &mut locals,
        );
        for (t, w) in ptys.iter_mut().zip(&wide) {
            *t = w.unwrap_or(*t);
        }
        // A module constant read in the body runs once the constant is initialized.
        self.cx.fn_value_depth += 1;
        let mut block = match body {
            ast::ArrowBody::Expr(x) if void_task => self.void_task_body(x),
            ast::ArrowBody::Expr(x) if await_body => self.awaited_body(x),
            _ => self.closure_body(body, span, ret.is_none()),
        };
        self.cx.fn_value_depth -= 1;
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
        // Reads outside the body's statements (an expression body) check at its start.
        let late = self.late_check_stmts(frame.late.checks.clone(), span);
        block.stmts.splice(0..0, late);
        let peers = self.group_peer_lets(&frame, span);
        block.stmts.splice(0..0, peers);
        if is_async {
            // A captured generator is checked once it is known whether the closure stays on
            // its task (`ownership::local_async`).
            self.no_captured_weak(&frame);
        }
        // Allowed while the closure stays on its task (`ownership::local_async`).
        let mutated = mutated_captures(&frame);
        let captures = self.capture_modes(&frame, span);
        let clause = throws.as_ref().map_or(span, |t| t.span);
        if declared_err.is_none() {
            self.provisional_slot(span, &frame.uncaught);
        }
        let err = self.closure_error(def, declared_err, &frame, clause);
        if rest {
            self.cx.fn_sites.rest_closures.insert(def);
        }
        let fn_ty = self.closure_type(ptys.clone(), rest, body_ret, err, is_async);
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
        crate::body::defaults::arrow_defaults(self.cx, self.module, def, params, true);
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

    /// An async closure that may run on another thread copies its captures when it runs: none
    /// may hold a weak collection.
    fn no_captured_weak(&mut self, frame: &Frame) {
        for c in &frame.captures {
            let l = &frame.locals[c.inner.0 as usize];
            if self.cx.holds_weak(l.ty) {
                let (ty, name, at) = (l.ty, l.name.clone(), l.span);
                self.no_weak_copy(ty, crate::body::GenCopy::Capture(name), at);
            }
        }
    }

    /// A generator function expression captures no generator (nor weak collection).
    pub(super) fn no_captured_generators(&mut self, frame: &Frame) {
        for c in &frame.captures {
            let l = &frame.locals[c.inner.0 as usize];
            let (ty, name, at) = (l.ty, l.name.clone(), l.span);
            self.no_generator_copy(ty, crate::body::GenCopy::Capture(name), at);
        }
    }

    pub(crate) fn alloc_closure(&mut self, span: Span) -> DefId {
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
        info.generics.constraints = self.constraints.clone();
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

    /// `from_context`: the arrow writes no result type.
    fn closure_body(
        &mut self,
        body: &ast::ArrowBody,
        span: Span,
        from_context: bool,
    ) -> hir::Block {
        match body {
            ast::ArrowBody::Expr(e) => {
                let h = match self.f.ret {
                    Some(_) if self.f.discards_value => self.expr_stmt(e),
                    // `(): void => void e` is `e;`, as in TypeScript.
                    Some(r) if r == self.cx.ty.unit && void_op(e).is_some() => {
                        self.expr_stmt(void_op(e).unwrap_or(e))
                    }
                    Some(r) => self.returned(e, r),
                    None => self.expr(e, None, Want::Move),
                };
                let h = match (self.f.ret, self.promise_value(h.ty)) {
                    // An async arrow giving a promise: its result is the promise's value.
                    (None, Some(t)) => {
                        self.f.ret = Some(t);
                        self.inferred_promise_return(h, t)
                    }
                    _ => h,
                };
                if self.f.ret.is_none() {
                    self.f.ret = Some(h.ty);
                }
                let hs = h.span;
                let adopt = std::mem::take(&mut self.f.adopt_stmt);
                let kind = if h.ty == self.cx.ty.unit || self.f.discards_value || adopt {
                    S::Expr(h)
                } else {
                    S::Return(Some(h))
                };
                let mut block = hir::Block {
                    stmts: vec![hir::Stmt { kind, span: hs }],
                    value: None,
                    span: e.span,
                };
                self.wrap_adopted(&mut block);
                block
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
                    None => self.finish_inferred_ret(&mut block),
                };
                self.check_returns("closure", ret, span, &mut block, from_context);
                self.wrap_adopted(&mut block);
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
            // A `const` captured while its initializer runs, and its flag: the cell itself,
            // which closures created inside this one share too (`late_consts`).
            let late = frame.late.caps.contains_key(&c.inner)
                || frame.late.caps.values().any(|r| *r == Some(c.inner));
            let escaping = escaping || late && !c.mutated;
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
        // A variable this closure modifies that the enclosing closure captured is modified by
        // that one too (#913): a handler or timer callback whose own closure assigns its capture
        // must stay on its task (`crate::ownership::local_async`).
        for c in &frame.captures {
            if let Some(at) = c.mutated_at {
                if let Some(p) = self.f.captures.iter_mut().find(|p| p.inner == c.outer) {
                    p.mutated_at.get_or_insert(at);
                }
            }
        }
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

/// The captured variables a closure's body modifies, with the first place each is
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
