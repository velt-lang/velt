//! The component value handed to the runtime. A named function is passed as an adapter closure
//! `(props) => Name(props)` (or `Name()` when it takes no props): a function value borrows its
//! argument, while a component may take ownership of its props (move `props.children`) or be
//! `async` (async functions own their arguments). The adapter's argument is a soft move: where
//! `Name` takes ownership, the adapter passes another reference to the props (a share: their
//! objects and elements are not copied, a pending async element included); where it borrows,
//! it passes them as they are. Any other function value is passed as it is (shared where its
//! variable is used again).

use velt_syntax::ast;

use super::component::{synthetic, ComponentFn};
use crate::body::places::is_place;
use crate::body::FnCx;
use crate::hir::{self, ExprKind as H, TyId};

/// Parameter name of the adapter closure.
const PROPS_PARAM: &str = "props";

impl FnCx<'_, '_> {
    /// The component as a value of the runtime's component type `expected`; `None` after a
    /// reported mismatch.
    pub(super) fn component_value(
        &mut self,
        func: ComponentFn,
        expected: TyId,
        el: &ast::JsxElement,
        tag: &str,
    ) -> Option<hir::Expr> {
        match func {
            ComponentFn::Named {
                callee,
                takes_props,
            } => Some(self.adapter(&callee, takes_props, expected, el)),
            ComponentFn::Value(v) => match self.try_coerce(v, expected) {
                Ok(v) => {
                    // A runtime that keeps the component owns it: a variable still used
                    // afterwards passes a copy (JS shares the function).
                    if is_place(&v) {
                        self.f.soft_moves.push(v.span);
                    }
                    Some(v)
                }
                Err(v) => {
                    let (f, e) = (self.cx.display(v.ty), self.cx.display(expected));
                    let why = format!(
                        "its type `{f}` is not assignable to `{e}`, the component type of the JSX provider"
                    );
                    self.not_component(tag, v.span, why);
                    None
                }
            },
        }
    }

    /// `(props) => Name(props)` typed `expected`.
    fn adapter(
        &mut self,
        callee: &ast::Expr,
        takes_props: bool,
        expected: TyId,
        el: &ast::JsxElement,
    ) -> hir::Expr {
        let span = callee.span;
        let param = ast::Ident {
            name: PROPS_PARAM.to_string(),
            span,
        };
        // The argument has the element's span, so it is the only place the soft move names.
        let arg_span = el.span;
        let args = if takes_props {
            let arg = ast::Ident {
                name: PROPS_PARAM.to_string(),
                span: arg_span,
            };
            vec![synthetic(ast::ExprKind::Ident(arg), arg_span)]
        } else {
            vec![]
        };
        let call = ast::ExprKind::Call {
            callee: Box::new(callee.clone()),
            type_args: vec![],
            args,
            optional: false,
        };
        let arrow = ast::ExprKind::Arrow {
            type_params: vec![],
            params: vec![ast::ArrowParam {
                name: param,
                pattern: None,
                ty: None,
                default: None,
                optional: false,
                rest: false,
            }],
            ret: None,
            throws: None,
            body: ast::ArrowBody::Expr(Box::new(synthetic(call, span))),
            is_async: false,
        };
        let h = self.closure(&synthetic(arrow, span), Some(expected), false);
        if let (H::Closure(c), true) = (&h.kind, takes_props) {
            self.cx.fn_info_mut(*c).soft_moves.push(arg_span);
        }
        h
    }
}
