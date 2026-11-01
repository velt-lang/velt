//! A value of an object type where an interface is expected (`const t: T = p` with
//! `type T = { get v(): number; set v(x: number) }` and `p: { v: number }`): TypeScript checks
//! the shape, so it fits. The interface value is the same object, through an implementation of
//! the interface for the object type (`crate::object_ifaces`) whose getter slots read its
//! fields, whose setter slots write them and whose methods call the functions its fields hold.
//!
//! Such an interface whose accessors come in pairs of one type (`get v(): T` with
//! `set v(x: T)`, or a getter alone) has the shape of an object type, `{ v: T }`: an object
//! literal of plain values for it (`{ v: 1 }`) is an object of that type converted to it, and
//! an interface value converts back to that object type (or one differing in `readonly`) as the
//! object it holds (`Downcast`). That is the same object only when every value of the interface
//! in the program holds an object of that type, which is checked once the bodies are
//! ([`check_unwraps`]): a class implementing it, or a literal with accessors, makes the
//! conversion an error.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::iface_object::Written;
use crate::body::FnCx;
use crate::ctx::Ctx;
use crate::hir::{self, DefId, ExprKind as H, TyId, TyKind};

/// The implementations made so far (`crate::object_ifaces`).
pub(crate) type ObjectImpls = crate::object_ifaces::ObjectImpls;

impl FnCx<'_, '_> {
    /// `h`, of an object type, as interface value `exp` (`iface` with `args`); `Err(h)` when
    /// the object type lacks a member (`Ctx::object_iface_impl`).
    pub(super) fn object_as_iface(
        &mut self,
        h: hir::Expr,
        exp: TyId,
        iface: DefId,
        args: &[TyId],
    ) -> Result<hir::Expr, hir::Expr> {
        let Some(index) = self.cx.object_iface_impl(h.ty, iface, args) else {
            return Err(h);
        };
        let mut h = h;
        self.force_move(&mut h);
        let span = h.span;
        let kind = H::ToDyn {
            expr: Box::new(h),
            impl_index: index,
        };
        Ok(self.mk(kind, exp, span))
    }
}

/// A conversion of an interface value to the object type of its shape (`iface_as_object`):
/// the interface, its arguments, the object type and where.
pub(crate) type Unwrap = (DefId, Vec<TyId>, TyId, Span);

impl FnCx<'_, '_> {
    /// The fields of the object type interface `iface<args>` spells out: each getter (with a
    /// setter of its type, or none) and field, in slot order; `None` when it has another member
    /// (a method, a setter alone or of another type, an optional field, a generic one).
    fn accessor_shape(&mut self, iface: DefId, args: &[TyId]) -> Option<Vec<(String, TyId)>> {
        let info = self.cx.iface(iface)?;
        let (methods, fields) = (info.methods.clone(), info.fields.clone());
        let mut full = args.to_vec();
        full.push(self.cx.ty.error);
        let mut shape: Vec<(String, TyId)> = vec![];
        let mut setters: Vec<(String, TyId)> = vec![];
        for m in &methods {
            if !m.generics.names.is_empty() || m.default.is_some() {
                return None;
            }
            match m.name.strip_prefix("set ") {
                Some(name) => {
                    let ty = self.cx.subst(m.params.first()?.ty, &full);
                    setters.push((name.to_string(), ty));
                }
                None if m.is_getter && m.params.is_empty() => {
                    shape.push((m.name.clone(), self.cx.subst(m.ret, &full)));
                }
                None => return None,
            }
        }
        for (name, ty) in &setters {
            if !shape
                .iter()
                .any(|(n, t)| n == name && self.cx.same_ty(*t, *ty))
            {
                return None;
            }
        }
        for f in &fields {
            if f.optional {
                return None;
            }
            shape.push((f.name.clone(), self.cx.subst(f.ty, &full)));
        }
        let typed = shape.iter().all(|(_, t)| !self.cx.ty.has_error(*t));
        typed.then_some(shape)
    }

    /// An object literal of plain values (`{ v: 1 }`) where interface `iface` (type `dyn_ty`)
    /// of only accessors and fields is expected: an object of the interface's shape, converted
    /// to it (see the module docs). `None` for another literal.
    pub(super) fn plain_literal_as_iface(
        &mut self,
        props: &[ast::ObjectProp],
        (iface, iargs, dyn_ty): &(DefId, Vec<TyId>, TyId),
        span: Span,
    ) -> Option<hir::Expr> {
        let mut keys = vec![];
        for p in props {
            match p {
                ast::ObjectProp::KeyValue(k, v)
                    if matches!(super::iface_object::written(Some(v)), Written::Value(_)) =>
                {
                    keys.push(k.name.clone())
                }
                ast::ObjectProp::Shorthand(k) => keys.push(k.name.clone()),
                _ => return None,
            }
        }
        if !iargs.is_empty() {
            return None;
        }
        let shape = self.accessor_shape(*iface, iargs)?;
        let mut names: Vec<String> = shape.iter().map(|(n, _)| n.clone()).collect();
        names.sort();
        keys.sort();
        if names != keys {
            return None;
        }
        let obj = self.cx.anon_type(&shape, self.module);
        let h = self.object_lit(props, Some(obj), span);
        let h = self.coerce(h, obj);
        Some(match self.object_as_iface(h, *dyn_ty, *iface, iargs) {
            Ok(h) => h,
            Err(h) => self.coerce(h, *dyn_ty),
        })
    }

    /// `h`, a value of an interface of only accessors and fields, as object type `exp`: the
    /// object of the interface's shape it holds, then converted to `exp` as any object of that
    /// type is (one differing in `readonly` is the same object). Checked against the program's
    /// implementations of the interface by [`check_unwraps`]. `Err(h)` for another conversion.
    pub(super) fn iface_as_object(
        &mut self,
        h: hir::Expr,
        exp: TyId,
    ) -> Result<hir::Expr, hir::Expr> {
        let TyKind::Dyn(iface, args) = self.cx.ty.kind(h.ty).clone() else {
            return Err(h);
        };
        if self.cx.dynamic(h.ty).is_some() || !self.cx.is_object_type(exp) || !args.is_empty() {
            return Err(h);
        }
        let Some(shape) = self.accessor_shape(iface, &args) else {
            return Err(h);
        };
        let full = self.cx.anon_type(&shape, self.module);
        let span = h.span;
        let mut h = h;
        self.force_move(&mut h);
        self.cx.unwraps.push((iface, args, full, span));
        let obj = self.mk(H::Downcast(Box::new(h)), full, span);
        if full == exp {
            return Ok(obj);
        }
        self.try_coerce(obj, exp)
    }
}

/// The conversions of interface values to the object type of their shape (`Ctx::unwraps`):
/// each is an error unless every implementation of the interface in the program is for that
/// object type (or one differing in `readonly`), so every value holds such an object. A value
/// of another interface converted to it (`Upcast`, `synth_ifaces`) holds an object of one of
/// that interface's implementations, so those count too.
pub(crate) fn check_unwraps(cx: &mut Ctx) {
    let unwraps = std::mem::take(&mut cx.unwraps);
    for (iface, args, full, span) in unwraps {
        let sources = upcast_sources(cx, iface);
        let tys: Vec<TyId> = (cx.impls.iter())
            .filter(|i| {
                (i.iface == iface && i.iface_args == args)
                    || (i.iface != iface && sources.contains(&i.iface))
            })
            .map(|i| i.ty)
            .collect();
        let Some(other) = tys
            .into_iter()
            .find(|&t| t != full && !cx.same_layout(t, full))
        else {
            continue;
        };
        let iname = cx.iface(iface).map(|i| i.name.clone()).unwrap_or_default();
        let to = cx.display(full);
        let literal =
            matches!(cx.ty.kind(other), TyKind::Adt(d, _) if cx.literal_structs.contains(d));
        let of = match literal {
            true => "an object literal with accessors".to_string(),
            false => format!("`{}`", cx.display(other)),
        };
        cx.error(
            Diagnostic::error(
                format!("a `{iname}` converts to `{to}` only as the object it holds, and this program also makes `{iname}` values of {of}, which is no `{to}`"),
                span,
            )
            .with_note(format!("a `{iname}` made from a `{to}` (an object literal of values, or a value of that type) converts back as the same object; one made from a class or from a literal with accessors would need a copy, which Velt does not make here"))
            .with_note(format!("build the object explicitly from the `{iname}`'s members, or keep the value a `{to}`")),
        );
    }
}

/// The interfaces whose values the program converts to interface `iface`, directly or through
/// others (`synth_ifaces` upcasts).
fn upcast_sources(cx: &Ctx, iface: DefId) -> Vec<DefId> {
    let edges = &cx.synth_ifaces.upcasts;
    let mut out: Vec<DefId> = vec![];
    let mut todo = vec![iface];
    while let Some(want) = todo.pop() {
        for &(have, _) in edges.iter().filter(|(h, w)| *w == want && *h != iface) {
            if !out.contains(&have) {
                out.push(have);
                todo.push(have);
            }
        }
    }
    out
}
