//! A function passed where a function type with more parameters is expected (`xs.map(double)`,
//! where `map` passes `(x, i)`): as in TS it is called with the leading arguments only. The
//! argument is wrapped in an arrow, `(p0, p1) => double(p0)`. Arrows themselves get unnamed
//! extra parameters instead (`closure`).

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
        &self,
        arg: &ast::Expr,
        expected: TyId,
    ) -> Option<ast::Expr> {
        let TyKind::FnPtr { params: want, .. } = self.cx.ty.kind(expected) else {
            return None;
        };
        let have = self.named_fn_arity(arg)?;
        if have >= want.len() {
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

    /// How many parameters the function `arg` names takes: a local of function type, or a
    /// non-generic module-level function.
    fn named_fn_arity(&self, arg: &ast::Expr) -> Option<usize> {
        let ast::ExprKind::Ident(id) = &arg.kind else {
            return None;
        };
        if let Some(t) = self.peek_local_ty(&id.name) {
            return match self.cx.ty.kind(t) {
                TyKind::FnPtr { params, .. } => Some(params.len()),
                _ => None,
            };
        }
        match self.cx.lookup_item_at(self.module, &id.name, id.span)? {
            Item::Def(d) if matches!(self.cx.info[d.0 as usize], DefInfo::Fn(_)) => {
                let f = self.cx.fn_info(d);
                (f.generics.len() == 0).then_some(f.params.len())
            }
            _ => None,
        }
    }
}
