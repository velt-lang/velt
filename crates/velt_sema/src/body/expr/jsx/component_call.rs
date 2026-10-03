//! The component value handed to the runtime. A named function is passed as an adapter closure
//! `(props) => Name(props)` (or `Name()` when it takes no props): a function value borrows its
//! argument, while a component may take ownership of its props (move `props.children`) or be
//! `async` (async functions own their arguments). The adapter's argument is a soft move: where
//! `Name` takes ownership, the adapter passes a copy of the props (until objects are shared
//! references); where it borrows, nothing is copied. Props that cannot be copied (they hold a
//! promise, e.g. a pending async element) are a compile error ([`check_prop_copies`]).
//! Any other function value is passed as it is (a copy where its variable is used again).

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::component::{synthetic, ComponentFn};
use crate::body::places::is_place;
use crate::body::FnCx;
use crate::ctx::Ctx;
use crate::hir::{self, DefId, ExprKind as H, PassMode, TyId};

/// Parameter name of the adapter closure.
const PROPS_PARAM: &str = "props";

/// An adapter passing props of type `props` to named component `component` (checked after
/// ownership inference).
pub(crate) struct Adapter {
    component: DefId,
    props: TyId,
    tag: String,
    span: Span,
}

impl FnCx<'_, '_> {
    /// The component as a value of the runtime's component type `expected`; `None` after a
    /// reported mismatch.
    pub(super) fn component_value(
        &mut self,
        func: ComponentFn,
        expected: TyId,
        props: TyId,
        el: &ast::JsxElement,
        tag: &str,
    ) -> Option<hir::Expr> {
        match func {
            ComponentFn::Named {
                def,
                callee,
                takes_props,
            } => Some(self.adapter(def, &callee, takes_props, expected, props, el, tag)),
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
    #[allow(clippy::too_many_arguments)] // the component, its call shape and the element
    fn adapter(
        &mut self,
        def: DefId,
        callee: &ast::Expr,
        takes_props: bool,
        expected: TyId,
        props: TyId,
        el: &ast::JsxElement,
        tag: &str,
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
                ty: None,
                default: None,
                optional: false,
            }],
            ret: None,
            throws: None,
            body: ast::ArrowBody::Expr(Box::new(synthetic(call, span))),
            is_async: false,
        };
        let h = self.closure(&synthetic(arrow, span), Some(expected), false);
        if let (H::Closure(c), true) = (&h.kind, takes_props) {
            self.cx.fn_info_mut(*c).soft_moves.push(arg_span);
            self.cx.jsx_adapters.push(Adapter {
                component: def,
                props,
                tag: tag.to_string(),
                span: el.name.as_ref().map_or(el.span, |n| n.span()),
            });
        }
        h
    }
}

/// After ownership inference: an adapter copies the props of a component that takes ownership
/// of them, which is impossible when they own a promise or a `[Symbol.dispose]` resource.
pub(crate) fn check_prop_copies(cx: &mut Ctx) {
    for a in std::mem::take(&mut cx.jsx_adapters) {
        let owned = cx
            .fn_info(a.component)
            .params
            .first()
            .is_some_and(|p| p.mode == PassMode::Owned);
        if !owned || cx.is_copy(a.props) || !cx.owns_resource(a.props) {
            continue;
        }
        let (name, shown) = (cx.fn_info(a.component).name.clone(), cx.display(a.props));
        cx.error(
            Diagnostic::error(
                format!("the props of <{}> cannot be copied into the component", a.tag),
                a.span,
            )
            .with_note(format!(
                "`{name}` takes ownership of its props, so the JSX provider calls it with a copy (until objects are shared references)"
            ))
            .with_note(format!(
                "`{shown}` holds a promise or a `[Symbol.dispose]` resource (e.g. a pending async element), which cannot be copied; read the props without moving fields out of them"
            )),
        );
    }
}
