//! A function passed where a function type with more parameters is expected (`xs.map(double)`,
//! where `map` passes `(x, i)`): as in TS it is called with the leading arguments only. The
//! argument is wrapped in an arrow, `(p0, p1) => double(p0)`. Arrows themselves get unnamed
//! extra parameters instead (`closure`). The same wrapper drops the result of a function passed
//! where a `void` one is expected (`each(xs, count)` for `f: (s: string) => void`), as TS
//! allows: the arrow's `void` comes from the expected type, so its body's value is dropped.

use velt_syntax::ast;

use crate::body::FnCx;
use crate::ctx::Item;
use crate::defs::DefInfo;
use crate::hir::{TyId, TyKind};

impl FnCx<'_, '_> {
    /// The arrow standing for the function named by `arg` where a function type with more
    /// parameters (`expected`) is expected. Only plain names (a function, or a local holding
    /// a function value): the wrapper evaluates `arg` again on each call.
    pub(super) fn fewer_params_adapter(
        &mut self,
        arg: &ast::Expr,
        expected: TyId,
    ) -> Option<ast::Expr> {
        // `f?: (s: string) => void` is `((s: string) => void) | null`: adapt to the function type.
        let expected = self.cx.ty.opt_payload(expected).unwrap_or(expected);
        let TyKind::FnPtr {
            params: want,
            ret: want_ret,
            ..
        } = self.cx.ty.kind(expected).clone()
        else {
            return None;
        };
        let to_void = want_ret == self.cx.ty.unit;
        let (have, ret) = self.named_fn_sig(arg, to_void)?;
        let drops_result = to_void
            && ret.is_some_and(|ret| {
                ret != self.cx.ty.unit && !self.cx.ty.is_bottom(ret) && !self.cx.ty.has_error(ret)
            });
        if have > want.len() || (have == want.len() && !drops_result) {
            return None;
        }
        let span = arg.span;
        let mk = |kind| ast::Expr {
            id: ast::NodeId(u32::MAX),
            kind,
            span,
        };
        let name = |k: usize| ast::Ident {
            name: format!("#arg{k}"),
            span,
        };
        let params = (0..want.len())
            .map(|k| ast::ArrowParam {
                name: name(k),
                ty: None,
                default: None,
                optional: false,
            })
            .collect();
        let call = mk(ast::ExprKind::Call {
            callee: Box::new(arg.clone()),
            type_args: vec![],
            args: (0..have)
                .map(|k| mk(ast::ExprKind::Ident(name(k))))
                .collect(),
            optional: false,
        });
        Some(mk(ast::ExprKind::Arrow {
            type_params: vec![],
            params,
            ret: None,
            throws: None,
            body: ast::ArrowBody::Expr(Box::new(call)),
            is_async: false,
        }))
    }

    /// How many parameters the function `arg` names takes, and its result type when `with_ret`
    /// (inferring it from the body if needed): a local of function type, or a non-generic
    /// module-level function.
    fn named_fn_sig(&mut self, arg: &ast::Expr, with_ret: bool) -> Option<(usize, Option<TyId>)> {
        let ast::ExprKind::Ident(id) = &arg.kind else {
            return None;
        };
        if let Some(t) = self.peek_local_ty(&id.name) {
            return match self.cx.ty.kind(t) {
                TyKind::FnPtr { params, ret, .. } => Some((params.len(), Some(*ret))),
                _ => None,
            };
        }
        match self.cx.lookup_item_at(self.module, &id.name, id.span)? {
            Item::Def(d) if matches!(self.cx.info[d.0 as usize], DefInfo::Fn(_)) => {
                let f = self.cx.fn_info(d);
                if f.generics.len() != 0 {
                    return None;
                }
                let n = f.params.len();
                let ret = with_ret.then(|| crate::body::returns::ret_of(self.cx, d, id.span));
                Some((n, ret))
            }
            _ => None,
        }
    }
}
