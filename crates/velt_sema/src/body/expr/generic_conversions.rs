//! Generic function values (#943) converted and called in other ways than `generic_values.rs`
//! makes them: as a function of fixed types (instantiated, `const p: (x: number) => number =
//! id`), a call's type arguments inferred from one (`xs.map(id)`), held as `any` or `Function`
//! (reported), and a static field holding a function value, called.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::any::Dynamic;
use crate::body::{FnCx, Want};
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
        // A function value may take fewer parameters than its type gives.
        let takes = self
            .cx
            .generic_sig(h.ty)
            .map_or(0, |g| g.method.params.len());
        let rest = rest && takes >= params.len();
        let params = &params[..params.len().min(takes)];
        let span = h.span;
        self.push_scope();
        let (ty, at) = (h.ty, h.span);
        let l = self.new_local(HELD, ty, false, at, crate::body::LocalKind::Temp);
        let scope = self.f.scopes.last_mut().expect("ICE: no scope");
        scope.names.insert(HELD.to_string(), l);
        let mut h = h;
        crate::body::places::set_place_mode(&mut h, hir::UseMode::Move);
        let stmts = vec![hir::Stmt {
            kind: hir::StmtKind::Let {
                local: l,
                init: Some(h),
            },
            span: at,
        }];
        let n = params.len();
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
            name: HELD.into(),
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
        let value = self.closure(&arrow, Some(exp), true);
        self.pop_scope();
        let value = self.coerce(value, exp);
        let block = hir::Block {
            stmts,
            value: Some(Box::new(value)),
            span,
        };
        Ok(self.mk(H::Block(block), exp, span))
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
