//! `f64.half(x)`: the static methods `extend` blocks add to primitive types (`extend f64 {
//! static half(x: f64): f64 { … } }`), called through the type's name (#1679), read as values
//! (`const h = f64.half;`, `f64.half?.(2)`), and through `this` in such a static (`this.one()`
//! is `f64.one()`). The name is no value otherwise: `const t = f64;` stays an error. Blanket
//! extensions (`extend<T extends I> T { static … }`) give no statics to a primitive type, as
//! they give none to a class.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::body::FnCx;
use crate::defs::{static_key, MethodRef};
use crate::hir::{self, Callee, ExprKind as H, TyId};

/// A primitive type named in an expression: its name as written and the type.
pub(super) type Prim<'n> = (&'n str, TyId);

/// The primitive type names that have statics through `T.name`.
const NAMES: [&str; 13] = [
    "i8", "i16", "i32", "i64", "isize", "u8", "u16", "u32", "u64", "usize", "f32", "f64", "bool",
];

impl FnCx<'_, '_> {
    /// The primitive type a name the program does not declare stands for in `T.name` (`i8` …
    /// `u64`, `isize`, `usize`, `f32`, `f64`, `bool`, `string`).
    pub(super) fn primitive_named(&mut self, name: &str) -> Option<TyId> {
        if !NAMES.contains(&name) && name != "string" {
            return None;
        }
        self.cx.ty.primitive(name)
    }

    /// In a static method of an `extend` block of a primitive type, that type (for `this`).
    pub(super) fn this_primitive(&mut self) -> Option<(String, TyId)> {
        let def = self.body_def?;
        let ty = self.cx.extensions.iter().find_map(|x| {
            let own = x.methods.values().any(|m| m.is_static && m.def == def);
            own.then_some(x.target)
        })?;
        let name = NAMES
            .into_iter()
            .chain(["string"])
            .find(|n| self.cx.ty.primitive(n) == Some(ty))?;
        Some((name.to_string(), ty))
    }

    /// `T.name(args)` for a primitive type name `T` (one the program doesn't declare), or
    /// `this.name(args)` in a static of `extend T`; `None` for other calls.
    pub(super) fn primitive_namespace_call(
        &mut self,
        object: &ast::Expr,
        prop: &ast::Ident,
        type_args: &[ast::TypeExpr],
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> Option<hir::Expr> {
        let (name, ty) = match &object.kind {
            ast::ExprKind::This => self.this_primitive()?,
            ast::ExprKind::Ident(id) if !self.is_local_name(&id.name) => {
                let ty = self.primitive_named(&id.name)?;
                if self.lookup_item(&id.name, id.span).is_some() {
                    return None;
                }
                (id.name.clone(), ty)
            }
            _ => return None,
        };
        let prim = (name.as_str(), ty);
        Some(self.primitive_static_call(prim, prop, type_args, args, exp, span))
    }

    /// `T.name(args)`: the static method `name` of primitive type `T`.
    pub(super) fn primitive_static_call(
        &mut self,
        (tname, ty): Prim,
        prop: &ast::Ident,
        type_args: &[ast::TypeExpr],
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        let Some(m) = self.primitive_static_or_report((tname, ty), prop) else {
            self.check_args_loose(args);
            return self.error_expr(span);
        };
        let name = &prop.name;
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
        self.mk(kind, ck.ret, span)
    }

    /// `T.name` as a value: the function of the static method `name` of primitive type `T`.
    pub(super) fn primitive_static_value(
        &mut self,
        (tname, ty): Prim,
        prop: &ast::Ident,
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        let Some(m) = self.primitive_static_or_report((tname, ty), prop) else {
            return self.error_expr(span);
        };
        crate::body::defaults::param_defaults(self.cx, m.def);
        let n = self.cx.fn_info(m.def).generics.len();
        let what = format!("{tname}.{}", prop.name);
        self.fn_ref_slots(m.def, &what, vec![None; n], exp, span)
    }

    /// The static method `prop` of primitive type `ty`, else a reported error.
    fn primitive_static_or_report(
        &mut self,
        (tname, ty): Prim,
        prop: &ast::Ident,
    ) -> Option<MethodRef> {
        let name = &prop.name;
        if let Some(m) = self.primitive_static(ty, name) {
            self.cx
                .rec_ref(prop.span, crate::ide::record::Target::Def(m.def));
            return Some(m);
        }
        let d = match self.primitive_method_exists(ty, name) {
            true => Diagnostic::error(
                format!(
                    "`{tname}.{name}` is an instance method: call it on a value (`x.{name}(…)`)"
                ),
                prop.span,
            ),
            false => Diagnostic::error(
                format!("`{tname}` has no static method `{name}`"),
                prop.span,
            )
            .with_note(format!(
                "declare one in an `extend {tname} {{ static {name}(…) {{ … }} }}` block"
            )),
        };
        self.cx.error(d);
        None
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
