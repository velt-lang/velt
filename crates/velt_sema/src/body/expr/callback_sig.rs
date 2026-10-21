//! What a callback adapter (`callback`) knows about the function it wraps: its parameters and
//! result (`named_fn_sig`, for a function passed by name: how many parameters it takes, how many
//! it needs, and what it returns), and the expected function type with the parameters still
//! being inferred filled in from them.

use velt_syntax::ast;

use velt_common::Diagnostic;

use super::callback_wrap::{plan, Plan, Shape};
use crate::body::FnCx;
use crate::ctx::Item;
use crate::defs::DefInfo;
use crate::hir::{DefId, TyId, TyKind};

/// What a callback adapter needs to know about the function a name refers to.
pub(super) struct FnSig {
    /// Its parameter types, how many of them must be passed (the others are optional, have
    /// defaults or are the rest parameter), and whether the last is a rest parameter.
    pub params: Vec<TyId>,
    pub required: usize,
    pub rest: bool,
    /// Its result type (`None` when not asked for).
    pub ret: Option<TyId>,
}

impl FnSig {
    /// Its parameters as a wrapper sees them.
    pub fn shape(&self) -> Shape {
        Shape::of(self.params.len(), self.rest)
    }
}

/// How many of `params` must be passed: those before the first with a default or the rest
/// parameter.
fn required(params: &[crate::defs::ParamSig], rest: bool) -> usize {
    let fixed = params.len() - usize::from(rest && !params.is_empty());
    params[..fixed]
        .iter()
        .take_while(|p| p.default.is_none())
        .count()
}

impl FnCx<'_, '_> {
    /// `exp` with each parameter whose type is still being inferred (`A` of
    /// `reduce<A>(f: (acc: A, x: T, i: i64) => A, init: A)`) taken from the wrapped function's
    /// parameters `own`, so the wrapper's parameters have types.
    pub(super) fn fill_unknown_params(&mut self, exp: TyId, own: &[TyId]) -> TyId {
        let TyKind::FnPtr {
            params,
            ret,
            throws,
            rest,
        } = self.cx.ty.kind(exp).clone()
        else {
            return exp;
        };
        let mut filled = params.clone();
        for (p, o) in filled.iter_mut().zip(own) {
            if self.cx.ty.has_error(*p) && !self.cx.ty.has_error(*o) {
                *p = *o;
            }
        }
        if filled == params {
            return exp;
        }
        self.cx.ty.intern(TyKind::FnPtr {
            params: filled,
            ret,
            throws,
            rest,
        })
    }

    /// The arguments a wrapper of shape `want` (whose rest arguments are `elem`s) passes to the
    /// function `sig` describes (`callback_wrap::plan`). A parameter it fills from a rest
    /// argument that may not have been passed gets `undefined` then, as in JavaScript: its
    /// type or `elem` must admit `undefined` (or, for a function `arg` names, it gets its
    /// default); `None` after reporting one that doesn't.
    pub(super) fn adapter_args(
        &mut self,
        want: Shape,
        elem: Option<TyId>,
        sig: &FnSig,
        arg: &ast::Expr,
    ) -> Option<Plan> {
        let span = arg.span;
        let own = sig.shape();
        let passed = match want.rest {
            true => own.fixed,
            false => own.fixed.min(want.fixed),
        };
        // A function value is called with every parameter (a call by name fills in defaults).
        let by_name = match &arg.kind {
            ast::ExprKind::Ident(id) => !self.is_local_name(&id.name),
            ast::ExprKind::Member { .. } => true,
            _ => false,
        };
        let required = if by_name { sig.required } else { own.fixed };
        let args = plan(want, own, passed, required);
        let read = args.may_be_missing();
        let admits =
            |cx: &crate::ctx::Ctx, t: TyId| cx.dynamic(t).is_some() || cx.ty.core(t).is_some();
        let bad = (want.fixed..want.fixed + read)
            .filter_map(|i| sig.params.get(i))
            .find(|t| !elem.is_some_and(|e| admits(self.cx, e)) && !admits(self.cx, **t));
        if let Some(&t) = bad {
            let shown = self.cx.display(t);
            self.cx.error(
                Diagnostic::error(
                    format!("this function takes a `{shown}` where the function type passes its rest arguments, which may be fewer"),
                    span,
                )
                .with_note("a missing argument would be `undefined`: give the parameter a type that admits it (`x?: T`, `any`, `unknown`), or take the arguments as a rest parameter (`...xs: T[]`)"),
            );
            return None;
        }
        Some(args)
    }

    /// The parameter types of a function value of type `t`.
    pub(super) fn fn_params_of(&self, t: TyId) -> Vec<TyId> {
        match self.cx.ty.kind(t) {
            TyKind::FnPtr { params, .. } => params.clone(),
            _ => vec![],
        }
    }

    /// The parameter types of the module-level function `callee` names (none when generic).
    pub(super) fn named_fn_params(&mut self, callee: &ast::Expr) -> Vec<TyId> {
        let ast::ExprKind::Ident(id) = &callee.kind else {
            return vec![];
        };
        match self.cx.lookup_item_at(self.module, &id.name, id.span) {
            Some(Item::Def(d)) if matches!(self.cx.info[d.0 as usize], DefInfo::Fn(_)) => {
                let f = self.cx.fn_info(d);
                match f.generics.len() {
                    0 => f.params.iter().map(|p| p.ty).collect(),
                    _ => vec![],
                }
            }
            _ => vec![],
        }
    }

    /// Whether a function returning `ret` fits where one returning `want` is expected only
    /// through its result: `want` is a union (or `T | null`) with `ret` as a member.
    pub(super) fn result_widens(&mut self, ret: TyId, want: TyId) -> bool {
        if ret == want {
            return false;
        }
        let inner = self.cx.ty.core(want).unwrap_or(want);
        inner == ret
            || self
                .cx
                .union_members(inner)
                .is_some_and(|ms| ms.contains(&ret))
    }

    /// The parameters (and with `with_ret` the result type, inferring it from the body if
    /// needed) of the function `arg` names: a local of function type (a closure `const` knows
    /// which of its parameters have defaults), or a non-generic module-level function.
    pub(super) fn named_fn_sig(&mut self, arg: &ast::Expr, with_ret: bool) -> Option<FnSig> {
        if let ast::ExprKind::Member {
            object,
            prop,
            optional: false,
        } = &arg.kind
        {
            return self.static_method_sig(object, prop, with_ret);
        }
        let ast::ExprKind::Ident(id) = &arg.kind else {
            return None;
        };
        if let Some(t) = self.peek_local_ty(&id.name) {
            let TyKind::FnPtr {
                params, ret, rest, ..
            } = self.cx.ty.kind(t).clone()
            else {
                return None;
            };
            let closure = self.local_closure_const(&id.name);
            let required = match closure {
                Some(c) => required(&self.cx.fn_info(c).params, rest),
                None => params.len() - usize::from(rest),
            };
            return Some(FnSig {
                params,
                required,
                rest,
                ret: Some(ret),
            });
        }
        match self.cx.lookup_item_at(self.module, &id.name, id.span)? {
            Item::Def(d)
                if matches!(self.cx.info[d.0 as usize], DefInfo::Fn(_))
                    && self.cx.fn_info(d).generics.len() == 0 =>
            {
                Some(self.def_sig(d, id.span, with_ret))
            }
            _ => None,
        }
    }

    /// The [`FnSig`] of the function `d`.
    fn def_sig(&mut self, d: DefId, span: velt_common::Span, with_ret: bool) -> FnSig {
        let rest = crate::body::defaults::has_rest(self.cx, d);
        let f = self.cx.fn_info(d);
        let params = f.params.iter().map(|p| p.ty).collect();
        let required = required(&f.params, rest);
        let ret = with_ret.then(|| self.callee_ret(d, span));
        FnSig {
            params,
            required,
            rest,
            ret,
        }
    }

    /// `C.f` naming a static method without type parameters of its own that doesn't use `this`
    /// (which can't be a value): like a module-level function in `named_fn_sig`. Also `this.f`
    /// in a static method, as the declaring class sees `f` (the wrapper calls `this.f(…)`).
    fn static_method_sig(
        &mut self,
        object: &ast::Expr,
        prop: &ast::Ident,
        with_ret: bool,
    ) -> Option<FnSig> {
        let (d, via_this) = match (&object.kind, self.static_this) {
            (ast::ExprKind::This, Some((_, declaring))) => (declaring, true),
            (ast::ExprKind::Ident(id), _) => {
                if self.peek_local_ty(&id.name).is_some() {
                    return None;
                }
                let Item::Def(d) = self.cx.lookup_item_at(self.module, &id.name, id.span)? else {
                    return None;
                };
                (d, false)
            }
            _ => return None,
        };
        self.cx.adt(d)?;
        let (m, owner_generics, _) = self.find_static(d, &prop.name).ok()?;
        let f = self.cx.fn_info(m.def);
        let uses_this = !via_this && self.cx.static_this.contains_key(&m.def);
        if f.generics.len() != owner_generics || uses_this {
            return None;
        }
        Some(self.def_sig(m.def, prop.span, with_ret))
    }

    /// What calling the named function `d` gives: its result type, a promise of it for an
    /// `async` function.
    pub(super) fn callee_ret(&mut self, d: DefId, span: velt_common::Span) -> TyId {
        let r = crate::body::returns::ret_of(self.cx, d, span);
        if self.is_async_fn(d) {
            self.async_call_ret(d, r)
        } else {
            r
        }
    }
}
