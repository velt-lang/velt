//! What a callback adapter (`callback`) knows about the function it wraps: its parameters and
//! result (`named_fn_sig`, for a function passed by name: how many parameters it takes, how many
//! it needs, and what it returns), and the expected function type with the parameters still
//! being inferred filled in from them.

use velt_syntax::ast;

use crate::body::FnCx;
use crate::ctx::Item;
use crate::defs::DefInfo;
use crate::hir::{DefId, TyId, TyKind};

/// What a callback adapter needs to know about the function a name refers to.
pub(super) struct FnSig {
    /// Its parameters, and how many of them must be passed (the rest are optional or have
    /// defaults).
    pub params: usize,
    pub required: usize,
    /// Its result type (`None` when not asked for).
    pub ret: Option<TyId>,
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
        })
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
        let inner = self.cx.ty.opt_payload(want).unwrap_or(want);
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
            let TyKind::FnPtr { params, ret, .. } = self.cx.ty.kind(t).clone() else {
                return None;
            };
            let closure = self.local_closure_const(&id.name);
            let required = match closure {
                Some(c) => {
                    let ps = &self.cx.fn_info(c).params;
                    ps.iter().take_while(|p| p.default.is_none()).count()
                }
                None => params.len(),
            };
            return Some(FnSig {
                params: params.len(),
                required,
                ret: Some(ret),
            });
        }
        match self.cx.lookup_item_at(self.module, &id.name, id.span)? {
            Item::Def(d) if matches!(self.cx.info[d.0 as usize], DefInfo::Fn(_)) => {
                let f = self.cx.fn_info(d);
                if f.generics.len() != 0 {
                    return None;
                }
                let params = f.params.len();
                let required = f.params.iter().take_while(|p| p.default.is_none()).count();
                let ret = with_ret.then(|| self.callee_ret(d, id.span));
                Some(FnSig {
                    params,
                    required,
                    ret,
                })
            }
            _ => None,
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
        let params = f.params.len();
        let required = f.params.iter().take_while(|p| p.default.is_none()).count();
        let ret = with_ret.then(|| self.callee_ret(m.def, prop.span));
        Some(FnSig {
            params,
            required,
            ret,
        })
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
