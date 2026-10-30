//! Generic function values (#943) converted and called in other ways than `generic_values.rs`
//! makes them: as a function of fixed types (instantiated, `const p: (x: number) => number =
//! id`), a call's type arguments inferred from one (`xs.map(id)`), held as `any` or `Function`
//! (reported), and a static field holding a function value, called.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::any::Dynamic;
use crate::body::{FnCx, Want};
use crate::defs::DefInfo;
use crate::hir::{self, DefId, ExprKind as H, TyId, TyKind};

/// The local a generic function value converted to a function type is held in
/// (`FnCx::generic_as_fn`): no source can name it.
const HELD: &str = "<generic value>";

/// The note of a function of fixed types (`f`) where generic function type `e` is expected.
pub(super) fn fixed_note(e: &str, f: &str) -> String {
    format!("a value of `{e}` takes any type arguments at each call; a function of fixed types (`{f}`) is not one: pass a generic function, or a closure that works for every type")
}

impl FnCx<'_, '_> {
    /// Generic function value `h` as a value of function type `exp`, as TypeScript instantiates
    /// it: the closure `(a0, …) => g(a0, …)` over `g`, a local holding `h`, whose call
    /// instantiates `h` at `exp`'s parameter types. `Err` when it doesn't take them.
    pub(super) fn generic_as_fn(
        &mut self,
        h: hir::Expr,
        exp: TyId,
    ) -> Result<hir::Expr, hir::Expr> {
        let TyKind::FnPtr { params, rest, .. } = self.cx.ty.kind(exp).clone() else {
            return Err(h);
        };
        if let Some(d) = self.named_fn_of(&h) {
            return Ok(self.named_fn_at(d, h.span, exp));
        }
        let mut g = self.forwarding(h, exp, (params.len(), rest), |s, arrow, exp| {
            s.closure(arrow, Some(exp), true)
        });
        self.mark_fresh(&mut g, exp);
        Ok(g)
    }

    /// A function held as `any` / `unknown` as a value of function type `exp` (of `n`
    /// parameters): the closure `(a0, …) => f(a0, …)` over `f`, a local holding the value (a
    /// rest parameter's elements passed as arguments of their own), whose call goes through
    /// `any` (the arguments converted to the held function's parameter types, a `TypeError` for
    /// one that doesn't convert, or for a value that is no function) and whose result converts
    /// to `exp`'s, as JavaScript calls the function whatever its declared types. Node checks nothing at the conversion either.
    /// Checked as a trial: `Err` (`h` as it was) when an argument or the result has no
    /// conversion through `any` (a result of `Promise<T>`), for the caller's checked
    /// conversion to the exact type.
    pub(super) fn dynamic_as_fn(
        &mut self,
        h: hir::Expr,
        exp: TyId,
        n: usize,
    ) -> Result<hir::Expr, hir::Expr> {
        let mark = crate::body::recheck::Mark::here(self.cx);
        let frames = self.trial_frames();
        let diags = self.cx.diags.len();
        let g = self.exact_or_forwarding(h.clone(), exp, n);
        if self.cx.diags.len() == diags {
            return Ok(g);
        }
        self.cx.diags.truncate(diags);
        mark.rollback(self.cx);
        self.restore_trial_frames(frames);
        Err(h)
    }

    /// The dynamic `h` as the function type `exp`: the function it holds when that is of
    /// exactly `exp` (`Intrinsic::AnyToExact`: the one value, so `===`, `Set` and `Map` keys
    /// see it, a round trip through `any` adds nothing, and a call costs what a call of the
    /// function costs), else the closure calling it through `any` (`dynamic_as_fn`). A
    /// variable is read in place, and only the closure takes a share of it.
    fn exact_or_forwarding(&mut self, h: hir::Expr, exp: TyId, n: usize) -> hir::Expr {
        use crate::body::places::{is_place, set_place_mode};
        let (dty, span) = (h.ty, h.span);
        let mut stmts = vec![];
        let (read, held) = if is_place(&h) {
            let mut read = h.clone();
            set_place_mode(&mut read, hir::UseMode::Borrow);
            (read, self.share_if_place(h))
        } else {
            let tmp = self.new_local("<any>", dty, false, span, crate::body::LocalKind::Temp);
            stmts.push(hir::Stmt {
                kind: hir::StmtKind::Let {
                    local: tmp,
                    init: Some(h),
                },
                span,
            });
            let read = self.mk(H::Local(tmp, hir::UseMode::Borrow), dty, span);
            (read, self.mk(H::Local(tmp, hir::UseMode::Move), dty, span))
        };
        // Called as an `any` (an `unknown` would have to be narrowed first).
        let held = match self.any_ty() {
            Some(any) if held.ty != any => {
                self.intrinsic(hir::Intrinsic::AnyTo, vec![held], any, span)
            }
            _ => held,
        };
        let opt = self.cx.ty.option_of(exp, hir::Nothing::Undefined);
        let exact = self.intrinsic(hir::Intrinsic::AnyToExact, vec![read], opt, span);
        let b = self.new_local("<exact>", exp, false, span, crate::body::LocalKind::Bind);
        let rest = matches!(self.cx.ty.kind(exp), TyKind::FnPtr { rest: true, .. });
        let adapter = self.forwarding(held, exp, (n, rest), |s, arrow, exp| {
            s.closure(arrow, Some(exp), true)
        });
        let pat = |kind| hir::Pat {
            kind,
            ty: opt,
            span,
        };
        let bind = hir::Pat {
            kind: hir::PatKind::Binding(b, hir::UseMode::Move),
            ty: exp,
            span,
        };
        let arms = vec![
            hir::Arm {
                pat: pat(hir::PatKind::Some(Box::new(bind))),
                guard: None,
                body: self.mk(H::Local(b, hir::UseMode::Move), exp, span),
            },
            hir::Arm {
                pat: pat(hir::PatKind::None),
                guard: None,
                body: adapter,
            },
        ];
        let m = H::Match {
            scrutinee: Box::new(exact),
            arms,
        };
        let value = self.mk(m, exp, span);
        if stmts.is_empty() {
            return value;
        }
        let block = hir::Block {
            stmts,
            value: Some(Box::new(value)),
            span,
        };
        self.mk(H::Block(block), exp, span)
    }

    /// `const g = <name>` of a named generic function (`const g: G = id`), or of a constant
    /// holding one: recorded, so a use of `g` as a function of fixed types is that function
    /// (`named_fn_of`).
    pub(in crate::body) fn note_named_fn(&mut self, v: &ast::VarDecl, name: &ast::Ident) {
        let Some(ast::ExprKind::Ident(init)) = v.init.as_ref().map(|e| &e.kind) else {
            return;
        };
        if v.kind != ast::VarKind::Const {
            return;
        }
        let d = match self.lookup_local(&init.name, init.span) {
            Some(l) => self.f.named_fns.get(&l).copied(),
            None => match self.cx.lookup_item_at(self.module, &init.name, init.span) {
                Some(crate::ctx::Item::Def(d)) => self.named_fn_def(d),
                _ => None,
            },
        };
        if let (Some(d), Some(l)) = (d, self.lookup_local(&name.name, name.span)) {
            self.f.named_fns.insert(l, d);
        }
    }

    /// The named generic function `h` (a read of a local or a module constant) holds
    /// (`note_named_fn`, `named_fn_def`).
    pub(super) fn named_fn_of(&mut self, h: &hir::Expr) -> Option<DefId> {
        match &h.kind {
            H::Local(l, _) => self.f.named_fns.get(l).copied(),
            H::Global(g) => self.named_fn_def(*g),
            H::Call {
                callee: hir::Callee::Intrinsic(hir::Intrinsic::Share),
                args,
            } => args.first().and_then(|a| self.named_fn_of(a)),
            _ => None,
        }
    }

    /// Generic function `d` itself, or the one module constant `d` holds through a chain of
    /// constants initialized with a name (`const gg: G = id; const hh: G = gg`).
    pub(super) fn named_fn_def(&self, mut d: DefId) -> Option<DefId> {
        for _ in 0..16 {
            match &self.cx.info[d.0 as usize] {
                DefInfo::Fn(f) if f.generics.len() > 0 => return Some(d),
                DefInfo::Global(g) if !g.mutable => {
                    let ast::ExprKind::Ident(name) = &g.src.init?.kind else {
                        return None;
                    };
                    let item = self.cx.lookup_item_at(g.module, &name.name, name.span);
                    let Some(crate::ctx::Item::Def(next)) = item else {
                        return None;
                    };
                    d = next;
                }
                _ => return None,
            }
        }
        None
    }

    /// Generic function `d` instantiated at the function type `exp`, as TypeScript keeps the one
    /// function where a constant holding it is used (`P === Q`, `p === id`).
    pub(super) fn named_fn_at(&mut self, d: DefId, span: Span, exp: TyId) -> hir::Expr {
        let name = self.cx.fn_info(d).name.clone();
        let id = ast::Ident {
            name: name.rsplit([':', '.']).next().unwrap_or(&name).to_string(),
            span,
        };
        let h = self.fn_ref(d, &id, Some(exp));
        self.coerce(h, exp)
    }

    /// Generic function value `h` as a value of another generic function type `exp` (one taking
    /// more optional parameters, or giving a wider result: `<T>(x: T, y?: number) => T`): the
    /// generic closure `(a0, …) => g(a0, …)` over `g`, a local holding `h`. A conversion between
    /// the instances of one type (`I<A>` as `I<B>`) is not this. `Err` when it doesn't convert.
    pub(super) fn generic_as_generic(
        &mut self,
        h: hir::Expr,
        exp: TyId,
    ) -> Result<hir::Expr, hir::Expr> {
        let (Some(from), Some(to)) = (self.cx.generic_sig(h.ty), self.cx.generic_sig(exp)) else {
            return Err(h);
        };
        if from.iface == to.iface {
            return Err(h);
        }
        // Checked as a trial: a value that doesn't convert is the caller's mismatch.
        let mark = crate::body::recheck::Mark::here(self.cx);
        let frames = self.trial_frames();
        let diags = self.cx.diags.len();
        let shape = (to.method.params.len(), to.rest);
        let mut g = self.forwarding(h.clone(), exp, shape, |s, arrow, exp| {
            s.generic_closure(arrow, Some(exp))
        });
        if self.cx.diags.len() == diags {
            self.mark_fresh(&mut g, exp);
            return Ok(g);
        }
        self.cx.diags.truncate(diags);
        mark.rollback(self.cx);
        self.restore_trial_frames(frames);
        Err(h)
    }

    /// Records the closure `h` makes as a fresh one, its values of type `ty`: each evaluation is
    /// a new function where JavaScript has one (`hir::Program::fresh_closures`).
    pub(super) fn mark_fresh(&mut self, h: &mut hir::Expr, ty: TyId) {
        if let Some(c) = crate::generic_fns::closure_in(h) {
            self.cx
                .generic_fns
                .fresh
                .insert(c, hir::FreshClosure::New(ty));
        }
    }

    /// Records the closure `h` makes as converting function values of type `from` to `to`
    /// (`hir::FreshClosure::Converts`): a fresh value converted by it may be compared as a `to`.
    pub(super) fn mark_converts(&mut self, h: &mut hir::Expr, from: TyId, to: TyId) {
        if let Some(c) = crate::generic_fns::closure_in(h) {
            let fresh = &mut self.cx.generic_fns.fresh;
            fresh
                .entry(c)
                .or_insert(hir::FreshClosure::Converts { from, to });
        }
    }

    /// `{ const g = h; (a0, …) => g(a0, …) }` as a value of `exp`, the arrow made by `make`:
    /// `n` parameters (the last a rest one if `rest`), as many of them passed as `h` takes (a
    /// function value may take fewer parameters than its type gives).
    fn forwarding(
        &mut self,
        h: hir::Expr,
        exp: TyId,
        (n, rest): (usize, bool),
        make: impl FnOnce(&mut Self, &ast::Expr, TyId) -> hir::Expr,
    ) -> hir::Expr {
        // A dynamic value (`dynamic_as_fn`) takes what it is called with.
        let takes = self
            .cx
            .generic_sig(h.ty)
            .map_or(n, |g| g.method.params.len());
        let rest = rest && takes >= n;
        let n = n.min(takes);
        let span = h.span;
        self.push_scope();
        let (ty, at) = (h.ty, h.span);
        // A dynamic value's local names it in a call's `TypeError` (`dynamic_as_fn`).
        let held_name = match self.cx.generic_sig(ty) {
            Some(_) => HELD.to_string(),
            None => "the value converted to a function type".to_string(),
        };
        let l = self.new_local(&held_name, ty, false, at, crate::body::LocalKind::Temp);
        let scope = self.f.scopes.last_mut().expect("ICE: no scope");
        scope.names.insert(held_name.clone(), l);
        let mut h = h;
        crate::body::places::set_place_mode(&mut h, hir::UseMode::Move);
        let stmts = vec![hir::Stmt {
            kind: hir::StmtKind::Let {
                local: l,
                init: Some(h),
            },
            span: at,
        }];
        let mk = |kind| ast::Expr {
            id: ast::NodeId(u32::MAX),
            kind,
            span,
        };
        let name = |k: usize| ast::Ident {
            name: format!("#a{k}"),
            span,
        };
        let ps = (0..n)
            .map(|k| ast::ArrowParam {
                name: name(k),
                pattern: None,
                ty: None,
                default: None,
                optional: false,
                rest: rest && k + 1 == n,
            })
            .collect();
        let args = (0..n)
            .map(|k| match rest && k + 1 == n {
                true => mk(ast::ExprKind::Spread(Box::new(mk(ast::ExprKind::Ident(
                    name(k),
                ))))),
                false => mk(ast::ExprKind::Ident(name(k))),
            })
            .collect();
        let held = ast::Ident {
            name: held_name,
            span,
        };
        let call = mk(ast::ExprKind::Call {
            callee: Box::new(mk(ast::ExprKind::Ident(held))),
            type_args: vec![],
            args,
            optional: false,
        });
        let arrow = mk(ast::ExprKind::Arrow {
            type_params: vec![],
            params: ps,
            ret: None,
            throws: None,
            body: ast::ArrowBody::Expr(Box::new(call)),
            is_async: false,
        });
        let value = make(self, &arrow, exp);
        self.pop_scope();
        let value = self.coerce(value, exp);
        let block = hir::Block {
            stmts,
            value: Some(Box::new(value)),
            span,
        };
        self.mk(H::Block(block), exp, span)
    }

    /// A generic function value (of type `g`) passed for a parameter of function type `pat`
    /// (over a call's type parameters, `slots` those known so far): the value is instantiated
    /// at `pat`'s parameter types (`generic_as_fn`), so its result there fixes the slots of
    /// `pat`'s result (`xs.map(id)` with `map<U>(f: (x: T) => U)`).
    pub(super) fn infer_from_generic(&mut self, pat: TyId, g: TyId, slots: &mut [Option<TyId>]) {
        let TyKind::FnPtr { params, ret, .. } = self.cx.ty.kind(pat).clone() else {
            return;
        };
        let Some(sig) = self.cx.generic_sig(g) else {
            return;
        };
        let known: Vec<TyId> = slots
            .iter()
            .map(|s| s.unwrap_or(self.cx.ty.error))
            .collect();
        let n = sig.args.len();
        let own = sig.method.generics.len();
        // Its own type parameters as `0..own`, its arguments in place.
        let mut map = sig.args.clone();
        map.push(self.cx.ty.error);
        map.extend((0..own).map(|k| self.cx.ty.param(k as u32)));
        let mut own_slots = vec![None; own];
        for (gp, pp) in sig.method.params.iter().zip(&params) {
            let gp = self.cx.subst(gp.ty, &map);
            let pp = self.cx.subst(*pp, &known);
            if !self.cx.ty.has_error(pp) {
                self.cx.match_ty(gp, pp, &mut own_slots);
            }
        }
        let Some(own_args) = own_slots.into_iter().collect::<Option<Vec<TyId>>>() else {
            return;
        };
        let mut inst = sig.args.clone();
        inst.resize(n, self.cx.ty.error);
        inst.push(self.cx.ty.error);
        inst.extend(own_args);
        let r = self.cx.subst(sig.method.ret, &inst);
        self.cx.match_ty(ret, r, slots);
    }

    /// `h` (of a static type) boxed as dynamic type `exp` (`d`); a generic function value
    /// where `any` or `Function` is expected is reported, as a call through them would need its
    /// type arguments.
    pub(super) fn box_value_dynamic(
        &mut self,
        h: hir::Expr,
        exp: TyId,
        d: Dynamic,
    ) -> Result<hir::Expr, hir::Expr> {
        if !(matches!(d, Dynamic::Any | Dynamic::Function) && self.cx.is_generic_fn(h.ty)) {
            return self.box_dynamic(h, exp);
        }
        let (tn, to) = (self.cx.display(h.ty), d.name());
        self.cx.error(
            Diagnostic::error(
                format!("a generic function value (`{tn}`) cannot be held as `{to}` yet"),
                h.span,
            )
            .with_note("a call through it would need the type arguments of each call (#943); hold it as `unknown` and convert it back with `as`, or keep its type"),
        );
        Ok(self.error_expr(h.span))
    }

    /// Is static field `prop` of class `d` (or a base class) of a function type, a generic
    /// function type or a callable object's?
    pub(super) fn static_fn_field(&mut self, d: DefId, prop: &ast::Ident) -> bool {
        let mut cur = Some(d);
        for _ in 0..64 {
            let Some(c) = cur else {
                return false;
            };
            if let Some(g) = self
                .cx
                .adt(c)
                .and_then(|a| a.statics.get(&prop.name).copied())
            {
                crate::body::driver::ensure_global(self.cx, g);
                let t = self.cx.global(g).map_or(self.cx.ty.error, |g| g.ty);
                let t = self.cx.ty.core(t).unwrap_or(t);
                return matches!(self.cx.ty.kind(t), TyKind::FnPtr { .. })
                    || self.cx.is_generic_fn(t)
                    || self.cx.is_callable_object(t);
            }
            cur = self
                .cx
                .adt(c)
                .and_then(|a| a.base)
                .and_then(|b| self.cx.class_of(b))
                .map(|(c, _)| c);
        }
        false
    }

    /// Generic function value `h` as a function taking `params` (a callback whose result type
    /// is not given: `p.then(f)`): instantiated at them (`generic_as_fn`); any other `h` as it is.
    pub(super) fn generic_callback(&mut self, h: hir::Expr, params: &[TyId]) -> hir::Expr {
        let Some(sig) = self.cx.generic_sig(h.ty) else {
            return h;
        };
        let own = sig.method.generics.len();
        let mut map = sig.args.clone();
        map.push(self.cx.ty.error);
        map.extend((0..own).map(|k| self.cx.ty.param(k as u32)));
        let mut slots = vec![None; own];
        for (gp, p) in sig.method.params.iter().zip(params) {
            let gp = self.cx.subst(gp.ty, &map);
            self.cx.match_ty(gp, *p, &mut slots);
        }
        let Some(own_args) = slots.into_iter().collect::<Option<Vec<TyId>>>() else {
            return h;
        };
        let mut inst = sig.args.clone();
        inst.push(self.cx.ty.error);
        inst.extend(own_args);
        let ps = (sig.method.params.iter().take(params.len()))
            .map(|p| self.cx.subst(p.ty, &inst))
            .collect();
        let ret = self.cx.subst(sig.method.ret, &inst);
        let throws = (sig.method.throws.and_then(|t| t.ty))
            .map_or(self.cx.ty.never, |t| self.cx.subst(t, &inst));
        let f = self.cx.ty.intern(TyKind::FnPtr {
            params: ps,
            ret,
            throws,
            rest: false,
        });
        self.coerce(h, f)
    }

    /// Does dynamic type `d` take a value of static type `t`? A generic function value is taken
    /// to be reported (`box_value_dynamic`) rather than mismatched.
    pub(super) fn takes_static(&mut self, d: Dynamic, t: TyId) -> bool {
        self.cx.is_generic_fn(t) || self.dynamic_takes_static(d, t)
    }

    /// An operand of `===` / `==` against an `any` or `unknown` (`dynamic_eq`), as an `any`: a
    /// generic function value too, which is compared, never called.
    pub(super) fn compared_operand(&mut self, h: hir::Expr, any: TyId) -> hir::Expr {
        if !self.cx.is_generic_fn(h.ty) {
            return self.dynamic_operand(h, any);
        }
        match self.box_dynamic(h, any) {
            Ok(b) => b,
            Err(h) => self.dynamic_operand(h, any),
        }
    }

    /// A call of function value `f` written with type arguments `type_args`, where `exp` is
    /// expected: a generic function value takes them (and is inferred against `exp`), as a call
    /// of a generic function does; any other value is called as `call_value` calls it.
    pub(super) fn call_value_with(
        &mut self,
        f: hir::Expr,
        type_args: &[ast::TypeExpr],
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        let Some(g) = self.cx.generic_sig(f.ty) else {
            return self.call_value(f, args, span);
        };
        let s = super::iface_call::IfaceSlot {
            on_param: false,
            iface: g.iface,
            iface_args: g.args,
            slot: 0,
            method: g.method,
            rest: g.rest,
        };
        self.iface_call(f, s, type_args, args, exp, span)
    }

    /// `C.f(args)` for a static field `f` of class `d` holding a function value.
    pub(super) fn static_field_call(
        &mut self,
        d: DefId,
        prop: &ast::Ident,
        args: &[ast::Expr],
        span: Span,
    ) -> hir::Expr {
        let f = self.static_field(d, prop, Want::Borrow, prop.span);
        let f = f.or_else(|| self.inherited_static_field(d, prop, Want::Borrow, prop.span));
        match f {
            Some(f) => self.call_value(f, args, span),
            None => self.error_expr(span),
        }
    }
}
