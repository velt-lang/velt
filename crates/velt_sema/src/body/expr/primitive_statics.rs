//! `f64.half(x)`: the static methods `extend` blocks add to primitive types (`extend f64 {
//! static half(x: f64): f64 { … } }`), called through the type's name (#1679). The name is no
//! value otherwise: `const t = f64;` stays an error.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::body::FnCx;
use crate::defs::{static_key, MethodRef};
use crate::hir::{self, Callee, ExprKind as H, TyId};

impl FnCx<'_, '_> {
    /// `T.name(args)` for a primitive type name `T` the program does not declare (`i8` …
    /// `u64`, `isize`, `usize`, `f32`, `f64`, `bool`, `string`); `None` for other names.
    pub(super) fn primitive_static_call(
        &mut self,
        id: &ast::Ident,
        prop: &ast::Ident,
        type_args: &[ast::TypeExpr],
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> Option<hir::Expr> {
        if !crate::types::velt_only_type_name(&id.name) && id.name != "string" {
            return None;
        }
        let ty = self.cx.ty.primitive(&id.name)?;
        let (tname, name) = (&id.name, &prop.name);
        let Some(m) = self.primitive_static(ty, name) else {
            let d = match self.primitive_method_exists(ty, name) {
                true => Diagnostic::error(
                    format!("`{tname}.{name}` is an instance method: call it on a value (`x.{name}(…)`)"),
                    prop.span,
                ),
                false => Diagnostic::error(format!("`{tname}` has no static method `{name}`"), prop.span)
                    .with_note(format!(
                        "declare one in an `extend {tname} {{ static {name}(…) {{ … }} }}` block"
                    )),
            };
            self.cx.error(d);
            self.check_args_loose(args);
            return Some(self.error_expr(span));
        };
        self.cx
            .rec_ref(prop.span, crate::ide::record::Target::Def(m.def));
        let c = self.fn_callable(m.def, format!("`{tname}.{name}`"), span);
        let n = c.slot_names.len();
        let mut slots = vec![None; n];
        self.explicit_type_args(&mut slots, n, type_args, &c.defaults, span);
        let ck = self.check_call(&c, slots, args, exp, span);
        self.note_async_args(m.def, &ck.args);
        self.call_throws(m.def, &ck.type_args, ck.ret, span);
        let kind = H::Call {
            callee: Callee::Def(m.def, ck.type_args),
            args: ck.args,
        };
        Some(self.mk(kind, ck.ret, span))
    }

    /// The static method `name` an `extend` block of primitive type `ty` declares.
    fn primitive_static(&self, ty: TyId, name: &str) -> Option<MethodRef> {
        let key = static_key(name);
        self.cx.extensions.iter().find_map(|x| {
            let ms = (x.target == ty && x.generics.len() == 0).then_some(&x.methods)?;
            ms.get(&key)
                .or_else(|| ms.get(name))
                .copied()
                .filter(|m| m.is_static)
        })
    }

    /// Does an `extend` block give primitive type `ty` an instance method `name`?
    fn primitive_method_exists(&self, ty: TyId, name: &str) -> bool {
        (self.cx.extensions.iter())
            .any(|x| x.target == ty && x.methods.get(name).is_some_and(|m| !m.is_static))
    }
}
