//! `String(x)`, template literals and string `+` of an `any`, `unknown` or `object` `x`
//! (issues #924, #1435): the text of the value held, as JavaScript's `ToString` writes it. A
//! class instance with a `toString()` (its own, a base class's, `Error`'s) is written as that
//! method returns; anything else goes to the prelude's `__anyText`, which writes an array as
//! `Array.prototype.toString` does and another object as `Object.prototype.toString` does
//! (`[object Object]`).

use velt_common::Span;
use velt_syntax::ast;

use super::setters::synth;
use crate::body::{FnCx, Want};
use crate::hir::{self, DefId};

impl FnCx<'_, '_> {
    /// The text of the dynamic value `h` (an `any`).
    pub(crate) fn dynamic_text(&mut self, h: hir::Expr, span: Span) -> hir::Expr {
        let init = self.share_if_place(h);
        // `__anyText` reads an array's elements through `any` (its own uses here don't count:
        // it runs only where a program's code calls it).
        if !self.cx.scopes[self.module].is_std {
            self.cx.note_dyn_members();
        }
        self.with_temp("<text>", init, span, span, |lw, v| {
            let s = lw.expr(v, None, Want::Borrow);
            let str_ = lw.cx.ty.str_;
            lw.dynamic_class_match_where(
                s,
                str_,
                "__anyText",
                |lw, c| lw.class_has_to_string(c),
                |lw, _, read| {
                    let prop = ast::Ident {
                        name: "toString".into(),
                        span,
                    };
                    let member = synth(
                        ast::ExprKind::Member {
                            object: Box::new(read),
                            prop,
                            optional: false,
                        },
                        span,
                    );
                    let call = synth(
                        ast::ExprKind::Call {
                            callee: Box::new(member),
                            type_args: vec![],
                            args: vec![],
                            optional: false,
                        },
                        span,
                    );
                    lw.prelude_fn_call("__anyText", vec![call], span)
                },
                span,
            )
        })
    }

    /// Does class `c` have a `toString()`, its own or a base class's?
    fn class_has_to_string(&mut self, c: DefId) -> bool {
        let n = self.cx.adt(c).map_or(0, |a| a.generics.len());
        let params: Vec<_> = (0..n as u32).map(|i| self.cx.ty.param(i)).collect();
        matches!(
            crate::collect::lookup_method(self.cx, c, &params, "toString"),
            Some(crate::collect::Found::Class { .. })
        )
    }
}
