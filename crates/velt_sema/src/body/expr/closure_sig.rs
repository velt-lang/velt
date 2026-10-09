//! The function type of an arrow: parameter and result types from annotations or the expected
//! function type, and its error type (written, expected, or what its body throws).

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::body::{FnCx, Frame};
use crate::hir::{DefId, TyId, TyKind};

/// The function type an arrow is checked against (each part `None` when unknown).
pub(super) struct Expected {
    pub params: Option<Vec<TyId>>,
    pub ret: Option<TyId>,
    /// What the arrow may throw (for an async arrow: the promise's rejection type).
    pub throws: Option<TyId>,
}

impl FnCx<'_, '_> {
    /// What the context expects of an arrow (from a function-type hint).
    pub(super) fn expected_fn(&mut self, exp: Option<TyId>, is_async: bool) -> Expected {
        let known = |s: &Self, t: TyId| (!s.cx.ty.has_error(t)).then_some(t);
        match self.hint(exp).map(|t| self.cx.ty.kind(t).clone()) {
            Some(TyKind::FnPtr {
                params,
                ret,
                throws,
            }) => {
                let throws = if is_async {
                    self.cx.ty.promise_error(ret).and_then(|e| known(self, e))
                } else {
                    known(self, throws)
                };
                Expected {
                    params: Some(params),
                    ret: Some(ret),
                    throws,
                }
            }
            _ => Expected {
                params: None,
                ret: None,
                throws: None,
            },
        }
    }

    /// Whether an async arrow is expected to return a type that is not a promise but includes
    /// exactly one (`View | Promise<View>`, `Promise<T> | null`): TS types the arrow by that
    /// member, so it is checked as `(params) => (async () => body)()` (`closure`), and the
    /// promise it returns converts to the expected type like any other value.
    pub(super) fn async_into_union(&mut self, exp: Option<TyId>) -> bool {
        let Some(TyKind::FnPtr { ret, .. }) = self.hint(exp).map(|t| self.cx.ty.kind(t).clone())
        else {
            return false;
        };
        if self.cx.ty.promise_payload(ret).is_some() || self.cx.ty.has_error(ret) {
            return false;
        }
        let inner = self.cx.ty.opt_payload(ret).unwrap_or(ret);
        if self.cx.ty.promise_payload(inner).is_some() {
            return true;
        }
        self.cx.union_members(inner).is_some_and(|ms| {
            ms.iter()
                .filter(|m| self.cx.ty.promise_payload(**m).is_some())
                .count()
                == 1
        })
    }

    /// Whether an async arrow is expected to be a `void` function (`onClick: () => void`): TS
    /// accepts it, and the promise each call starts runs to completion on its own. It is
    /// checked as `(params) => { const p = (async () => body)(); }` (`closure`).
    pub(super) fn async_into_void(&mut self, exp: Option<TyId>) -> bool {
        matches!(
            self.hint(exp).map(|t| self.cx.ty.kind(t).clone()),
            Some(TyKind::FnPtr { ret, .. }) if ret == self.cx.ty.unit
        )
    }

    /// The member of a union of function types (`(() => void) | (() => Promise<void>)`) that
    /// an arrow with `n_params` parameters is typed by, as TS's contextual typing picks one: an
    /// async arrow takes a member returning a promise (or a union with one), else a `void` one;
    /// a sync arrow the first member not returning a promise.
    pub(super) fn arrow_member(
        &mut self,
        exp: Option<TyId>,
        is_async: bool,
        n_params: usize,
    ) -> Option<TyId> {
        let u = self.hint(exp)?;
        let members = self.cx.union_members(u)?;
        let fns: Vec<(TyId, TyId)> = members
            .iter()
            .filter_map(|m| match self.cx.ty.kind(*m) {
                TyKind::FnPtr { params, ret, .. } if params.len() >= n_params => Some((*m, *ret)),
                _ => None,
            })
            .collect();
        let promise = |s: &mut Self, ret: TyId| {
            s.cx.ty.promise_payload(ret).is_some()
                || s.cx
                    .union_members(ret)
                    .is_some_and(|ms| ms.iter().any(|m| s.cx.ty.promise_payload(*m).is_some()))
        };
        let unit = self.cx.ty.unit;
        if is_async {
            for &(m, ret) in &fns {
                if promise(self, ret) {
                    return Some(m);
                }
            }
            return fns.iter().find(|(_, ret)| *ret == unit).map(|(m, _)| *m);
        }
        for &(m, ret) in &fns {
            if !promise(self, ret) {
                return Some(m);
            }
        }
        None
    }

    /// The closure's error type: written or expected (`declared`), else what its body is known
    /// to throw now (re-checked once every error type is inferred, `crate::throws`).
    pub(super) fn closure_error(
        &mut self,
        def: DefId,
        declared: Option<TyId>,
        frame: &Frame,
        span: Span,
    ) -> Option<TyId> {
        let from_body = declared.is_none();
        let ty = match declared {
            Some(t) => self.cx.canon_error(Some(t)),
            None => crate::throws::srcs_now(self.cx, &frame.uncaught),
        };
        self.cx.fn_info_mut(def).declared_throws = Some(crate::defs::DeclaredThrows {
            ty,
            span,
            from_body,
        });
        ty
    }

    /// `(params) => ret throws err`, or for an async arrow `(params) => Promise<ret, err>`.
    pub(super) fn closure_type(
        &mut self,
        params: Vec<TyId>,
        ret: TyId,
        err: Option<TyId>,
        is_async: bool,
    ) -> TyId {
        let never = self.cx.ty.never;
        let err = err.unwrap_or(never);
        let (ret, throws) = if is_async {
            (self.cx.ty.promise_rejecting(ret, err), never)
        } else {
            (ret, err)
        };
        self.cx.ty.intern(TyKind::FnPtr {
            params,
            ret,
            throws,
        })
    }

    /// The declared return type of an arrow (`Promise<T>` → `T` for async arrows).
    pub(super) fn closure_ret_annotation(&mut self, t: &ast::TypeExpr, is_async: bool) -> TyId {
        let r = self.resolve(t);
        if !is_async || self.cx.ty.is_bottom(r) {
            return r;
        }
        match self.cx.ty.promise_payload(r) {
            Some(p) => p,
            None => {
                let found = self.cx.display(r);
                self.cx.error(
                    Diagnostic::error(
                        "the return type of an async function must be `Promise<T>`",
                        t.span,
                    )
                    .with_note(format!("write `Promise<{found}>`")),
                );
                r
            }
        }
    }

    pub(super) fn closure_param_types(
        &mut self,
        params: &[ast::ArrowParam],
        exp: Option<&[TyId]>,
    ) -> Vec<TyId> {
        let mut out = vec![];
        for (i, p) in params.iter().enumerate() {
            let from_ctx = exp
                .and_then(|ps| ps.get(i).copied())
                .filter(|t| !self.cx.ty.has_error(*t));
            let ty = match (&p.ty, from_ctx) {
                (Some(t), _) => self.resolve(t),
                (None, Some(t)) => t,
                (None, None) if p.default.is_some() => {
                    let e = p.default.as_ref().expect("ICE: checked above");
                    crate::body::defaults::default_type(self.cx, self.module, e)
                }
                (None, None) => {
                    self.cx.error(
                        Diagnostic::error(format!("type annotations needed for parameter `{}`", p.name.name), p.name.span)
                            .with_note("annotate it (`(x: i64) => ...`) or pass the function where a function type is expected"),
                    );
                    self.cx.ty.error
                }
            };
            out.push(ty);
        }
        out
    }
}
