//! Classes as plain values (#926, #925): `x.constructor` of an `any`, `object` or `Function`
//! `x`, and a class named where an `any`, `unknown`, `object` or `Function` is expected, as in
//! `const ctor = value.constructor; ctor === Set || ctor === MyClass`.
//!
//! A class's object is a function constant of the module state (made on first use, kept in
//! `objects.class_values` under the type `() => any`, initialized before any module runs, as
//! the construct-signature values of `ctor_values.rs` are), so `===`, `Map` keys and `Set`
//! members see one object per class, and `typeof` is `"function"`. Calling it stops with
//! Node's `TypeError` for a class called without `new`; `new` through it is not supported yet.
//! `x.constructor` picks the object of the class `x` holds (a subclass's when it was converted
//! through its base); a value that holds no class instance (an object literal, an array, a
//! primitive) gives a function of its own, which equals no class.

use velt_common::Span;
use velt_syntax::ast;

use super::setters::synth;
use crate::any::Dynamic;
use crate::body::{FnCx, Want};
use crate::defs::{BodyState, DefInfo, GlobalInfo, GlobalSrc};
use crate::hir::{self, AdtKind, DefId, TyId};

impl FnCx<'_, '_> {
    /// Class `d` named where a value of the dynamic type `exp` is expected: its object
    /// (module docs). `None` when `exp` is not dynamic or `d` is not a class.
    pub(super) fn class_object_value(
        &mut self,
        d: DefId,
        exp: Option<TyId>,
        want: Want,
        span: Span,
    ) -> Option<hir::Expr> {
        let exp = exp?;
        self.cx.dynamic(exp)?;
        let h = self.class_object(d, want, span)?;
        Some(self.coerce(h, exp))
    }

    /// `x.constructor` of the dynamic `obj`: the object of the class it holds (module docs).
    /// `None` when `obj` is not an `any`, `object` or `Function`.
    pub(crate) fn dynamic_constructor(
        &mut self,
        obj: hir::Expr,
        span: Span,
    ) -> Result<hir::Expr, hir::Expr> {
        if !matches!(
            self.cx.dynamic(obj.ty),
            Some(Dynamic::Any | Dynamic::Object | Dynamic::Function)
        ) {
            return Err(obj);
        }
        let Some(any) = self.any_ty() else {
            return Ok(self.error_expr(span));
        };
        let h = self.dynamic_class_match(
            obj,
            any,
            "__anyCtor",
            |lw, c, _| match lw.class_object(c, Want::Move, span) {
                Some(h) => lw.coerce(h, any),
                None => lw.error_expr(span),
            },
            span,
        );
        Ok(h)
    }

    /// A read of class `d`'s object, made on first use (module docs).
    fn class_object(&mut self, d: DefId, want: Want, span: Span) -> Option<hir::Expr> {
        let a = self.cx.adt(d).filter(|a| a.kind == AdtKind::Class)?;
        let (module, name, qual, decl) = (a.module, a.name.clone(), a.qual_name.clone(), a.span);
        let std = self.cx.scopes[module].is_std;
        // `() => any`, which a call through an `any` can call (a `never` result has no box).
        let ret = self.any_ty()?;
        let ty = self.cx.ty.fn_ptr(vec![], ret);
        let key = (d, ty);
        if let Some((_, g)) = self.cx.objects.class_values.iter().find(|(k, _)| *k == key) {
            let g = *g;
            return Some(self.global_read(g, want, span));
        }
        let shown = name.rsplit('.').next().unwrap_or(&name).to_string();
        let msg = match std {
            true => format!("TypeError: Constructor {shown} requires 'new'"),
            false => {
                format!("TypeError: Class constructor {shown} cannot be invoked without 'new'")
            }
        };
        let info = GlobalInfo {
            name: format!("{name}#ctor"),
            qual_name: format!("{qual}#ctor"),
            module,
            span: Span::DUMMY,
            ty,
            init: None,
            state: BodyState::InProgress,
            src: GlobalSrc {
                ann: None,
                init: None,
                span: decl,
                owner: None,
                is_private: false,
            },
            mutable: false,
            stored: true,
        };
        let g = self
            .cx
            .alloc_def(Span::DUMMY, DefInfo::Global(Box::new(info)));
        self.cx.objects.class_values.push((key, g));
        let call = synth(
            ast::ExprKind::Call {
                callee: Box::new(synth(
                    ast::ExprKind::Ident(ast::Ident {
                        name: "__classCall".into(),
                        span,
                    }),
                    span,
                )),
                type_args: vec![],
                args: vec![synth(ast::ExprKind::Lit(ast::Lit::Str(msg)), span)],
                optional: false,
            },
            span,
        );
        let arrow = synth(
            ast::ExprKind::Arrow {
                type_params: vec![],
                params: vec![],
                ret: None,
                throws: None,
                body: ast::ArrowBody::Expr(Box::new(call)),
                is_async: false,
            },
            span,
        );
        let init = {
            let mut fcx = crate::body::driver::detached(self.cx, module, &[]);
            fcx.fn_name = format!("{qual}#ctor");
            let h = fcx.expr(&arrow, Some(ty), Want::Move);
            fcx.coerce(h, ty)
        };
        if let DefInfo::Global(info) = &mut self.cx.info[g.0 as usize] {
            info.init = Some(init);
            info.state = BodyState::Done;
        }
        Some(self.global_read(g, want, span))
    }
}
