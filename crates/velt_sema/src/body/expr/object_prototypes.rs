//! Prototypes as values (object_builtins.rs): `Object.getPrototypeOf(v)`, `Object.prototype` and
//! `C.prototype` are `ObjectPrototype`s (std/prelude/record.vlt) naming the prototype by a tag,
//! which `===` compares (`__prototypeSame`, called from `binary`), and which print, convert to a
//! string and to JSON as Node does. A record made by `Object.create(null)` has none (`null`,
//! tested at run time), a class with subclasses the one of the class it holds.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::setters::synth;
use crate::body::{FnCx, Want};
use crate::ctx::Item;
use crate::hir::{self, AdtKind, DefId, ExprKind as H, PatKind as P, TyId, TyKind};

/// A prototype Velt names: what tells it apart, and what `console.log`, `String` and
/// `JSON.stringify` make of it, as in Node.
struct Proto {
    tag: String,
    shown: String,
    text: String,
    array: bool,
}

impl Proto {
    fn new(tag: &str, shown: &str, text: &str) -> Proto {
        Proto {
            tag: tag.to_string(),
            shown: shown.to_string(),
            text: text.to_string(),
            array: false,
        }
    }

    /// `Object.prototype`.
    fn object() -> Proto {
        Proto::new("Object", "[Object: null prototype] {}", "[object Object]")
    }

    /// The prototype of a built-in collection, by its class name (`Map`, `Set`, ...).
    fn builtin(name: &str) -> Option<Proto> {
        Some(match name {
            "Array" => Proto {
                array: true,
                ..Proto::new("Array", "Object(0) []", "")
            },
            "Map" | "Set" | "WeakMap" | "WeakSet" | "Promise" => Proto::new(
                name,
                &format!("Object [{name}] {{}}"),
                &format!("[object {name}]"),
            ),
            _ => return None,
        })
    }
}

/// How the prototype of a value of one type is found.
enum ProtoOf {
    /// Known from the type.
    Static(Proto),
    /// A class with subclasses, or an interface value: tested against these classes (deepest
    /// first), then the static type's own prototype (if a class).
    Classes(Vec<DefId>, Option<Proto>),
}

impl FnCx<'_, '_> {
    /// `X.prototype` or `X.prototype.m` read (`None`: another member read).
    pub(crate) fn prototype_read(
        &mut self,
        object: &ast::Expr,
        prop: &ast::Ident,
        optional: bool,
        span: Span,
    ) -> Option<hir::Expr> {
        if optional {
            return None;
        }
        match &object.kind {
            ast::ExprKind::Ident(id) if prop.name == "prototype" => self.prototype_member(id, span),
            _ => self.prototype_method_value(object, prop, span),
        }
    }

    /// `Object.getPrototypeOf(v)`.
    pub(super) fn get_prototype_of(&mut self, args: &[ast::Expr], span: Span) -> hir::Expr {
        let [v] = args else {
            return self.arity_error("Object.getPrototypeOf", 1, args, span);
        };
        let s = self.expr(v, None, Want::Borrow);
        if self.cx.ty.is_bottom(s.ty) {
            return self.error_expr(span);
        }
        let Some(proto_ty) = self.prototype_ty(span) else {
            return self.error_expr(span);
        };
        let ret = self.cx.ty.option(proto_ty);
        let sty = s.ty;
        if self.spreads_dynamic(sty) && self.cx.ty.opt_payload(sty).is_none() {
            return self.dynamic_prototype(s, ret, span);
        }
        if self.cx.ty.opt_payload(sty).is_some() {
            let tn = self.cx.display(sty);
            self.cx.error(
                Diagnostic::error(
                    format!("`Object.getPrototypeOf` of a `{tn}` is not supported yet"),
                    v.span,
                )
                .with_note("test for `null` first (Node throws a `TypeError` for `null`)"),
            );
            return self.error_expr(span);
        }
        let members = self.cx.union_members(sty);
        let parts = members.clone().unwrap_or_else(|| vec![sty]);
        let mut found = vec![];
        for t in &parts {
            match self.proto_of(*t) {
                Some(p) => found.push(p),
                None => {
                    let tn = self.cx.display(*t);
                    self.cx.error(
                        Diagnostic::error(
                            format!("`Object.getPrototypeOf` of a `{tn}` is not supported yet"),
                            v.span,
                        )
                        .with_note("Velt names the prototypes of objects, records, class instances, arrays, `Map`, `Set`, `WeakMap`, `WeakSet` and promises"),
                    );
                    return self.error_expr(span);
                }
            }
        }
        if members.is_some() {
            // A union: one arm per member (a member that needs a test of its own is bound).
            let def = self.cx.union_def(sty).map(|(d, _)| d);
            let Some(def) = def else {
                return self.error_expr(span);
            };
            let mut arms = vec![];
            for (v, (t, p)) in parts.iter().zip(found).enumerate() {
                let l = self.new_local(
                    &format!("<proto@{}:{v}>", span.lo),
                    *t,
                    false,
                    span,
                    crate::body::LocalKind::Temp,
                );
                let mode = match self.cx.is_copy(*t) {
                    true => hir::UseMode::Copy,
                    false => hir::UseMode::Borrow,
                };
                let inner = self.pat(P::Binding(l, mode), *t, span);
                let pat = self.pat(
                    P::Variant {
                        def,
                        variant: v as u32,
                        args: vec![inner],
                    },
                    sty,
                    span,
                );
                let body = self.proto_value(l, *t, p, ret, span);
                arms.push(hir::Arm {
                    pat,
                    guard: None,
                    body,
                });
            }
            let kind = H::Match {
                scrutinee: Box::new(s),
                arms,
            };
            return self.mk(kind, ret, span);
        }
        let p = found.pop().expect("ICE: one prototype");
        let (l, mode) = self.option_binding(&s, sty, "<proto>", false);
        let body = self.proto_value(l, sty, p, ret, span);
        let pat = self.pat(P::Binding(l, mode), sty, span);
        let kind = H::Match {
            scrutinee: Box::new(s),
            arms: vec![hir::Arm {
                pat,
                guard: None,
                body,
            }],
        };
        self.mk(kind, ret, span)
    }

    /// `Object.getPrototypeOf(v)` of an `any` or `object` `v` (of type `ret`): its class's
    /// prototype, a record's (`null` when made by `Object.create(null)`), else the prelude's
    /// `__anyPrototype(v)` (arrays, objects; anything else is a `TypeError`).
    fn dynamic_prototype(&mut self, s: hir::Expr, ret: TyId, span: Span) -> hir::Expr {
        let record = self.cx.prelude_adt("Record");
        self.dynamic_class_match(
            s,
            ret,
            "__anyPrototype",
            |lw, c, read| {
                if Some(c) == record {
                    return lw.prelude_fn_call("__anyRecordPrototype", vec![read], span);
                }
                match lw.class_proto(c) {
                    Some(p) => lw.new_prototype(&p, ret, span),
                    None => {
                        let name = lw.cx.adt(c).map_or(String::new(), |a| a.name.clone());
                        let lit = synth(ast::ExprKind::Lit(ast::Lit::Str(name)), span);
                        lw.prelude_fn_call("__anyPrototypeUnsupported", vec![lit], span)
                    }
                }
            },
            span,
        )
    }

    /// How the prototype of a value of type `t` (not a union) is found (`None`: Velt doesn't
    /// name it).
    fn proto_of(&mut self, t: TyId) -> Option<ProtoOf> {
        match self.cx.ty.kind(t).clone() {
            TyKind::Array(_) => Some(ProtoOf::Static(Proto::builtin("Array")?)),
            TyKind::Promise(..) => Some(ProtoOf::Static(Proto::builtin("Promise")?)),
            TyKind::Dyn(i, _) => {
                let classes = self.iface_classes(i, t, Span::DUMMY)?;
                Some(ProtoOf::Classes(classes, None))
            }
            TyKind::Adt(d, _) => {
                let a = self.cx.adt(d)?;
                if a.kind == AdtKind::Anon {
                    return Some(ProtoOf::Static(Proto::object()));
                }
                if self.record_args(t).is_some() {
                    // A record made by `Object.create(null)` has none: tested at run time.
                    return Some(ProtoOf::Static(Proto::object()));
                }
                if a.kind != AdtKind::Class {
                    return None;
                }
                let own = self.class_proto(d)?;
                match self.class_subclasses(t) {
                    None => Some(ProtoOf::Static(own)),
                    Some(subs) => {
                        // Every subclass must have a prototype Velt names too.
                        for s in &subs {
                            self.class_proto(*s)?;
                        }
                        Some(ProtoOf::Classes(subs, Some(own)))
                    }
                }
            }
            _ => None,
        }
    }

    /// The prototype of class `d`'s instances (`None`: a standard library class Velt doesn't
    /// name, or a class extending one).
    fn class_proto(&mut self, d: DefId) -> Option<Proto> {
        let a = self.cx.adt(d)?;
        if self.cx.scopes[a.module].is_std {
            let name = a.name.clone();
            return Proto::builtin(&name)
                .filter(|_| self.cx.prelude_adt(&name) == Some(d) || name == "Set");
        }
        let base = a.base.and_then(|b| self.cx.class_of(b)).map(|(b, _)| b);
        let shown = match base {
            None => "{}".to_string(),
            Some(b) => {
                // A class extending a standard library class: not named yet.
                let ba = self.cx.adt(b)?;
                if self.cx.scopes[ba.module].is_std {
                    return None;
                }
                format!("{} {{}}", ba.name)
            }
        };
        Some(Proto {
            tag: format!("class#{}", d.0),
            shown,
            text: "[object Object]".to_string(),
            array: false,
        })
    }

    /// The prelude's `ObjectPrototype` type (`None`, reported: no prelude).
    fn prototype_ty(&mut self, span: Span) -> Option<TyId> {
        let Some(d) = self.cx.prelude_adt("ObjectPrototype") else {
            self.cx
                .err("prototypes need the prelude (std/prelude)", span);
            return None;
        };
        Some(self.cx.ty.intern(TyKind::Adt(d, vec![])))
    }

    /// `new ObjectPrototype(...)` for `p`, of type `ty` (`ObjectPrototype` or `| null`).
    fn new_prototype(&mut self, p: &Proto, ty: TyId, span: Span) -> hir::Expr {
        let lit = |s: &str| synth(ast::ExprKind::Lit(ast::Lit::Str(s.to_string())), span);
        let e = synth(
            ast::ExprKind::New {
                class: ast::TypeExpr {
                    kind: ast::TypeExprKind::Named {
                        path: vec![ast::Ident {
                            name: "ObjectPrototype".to_string(),
                            span,
                        }],
                        args: vec![],
                    },
                    span,
                },
                args: vec![
                    lit(&p.tag),
                    lit(&p.shown),
                    lit(&p.text),
                    synth(ast::ExprKind::Lit(ast::Lit::Bool(p.array)), span),
                ],
            },
            span,
        );
        self.expr(&e, Some(ty), Want::Move)
    }

    /// The prototype of the value bound to `l` (of type `t`), found as `how` says, as a value
    /// of type `ret`.
    fn proto_value(
        &mut self,
        l: hir::LocalId,
        t: TyId,
        how: ProtoOf,
        ret: TyId,
        span: Span,
    ) -> hir::Expr {
        match how {
            ProtoOf::Static(p) if self.record_args(t).is_some() => {
                // `r.__isNullProto() ? null : Object.prototype`.
                let obj = self.mk(H::Local(l, hir::UseMode::Borrow), t, span);
                let test = self.record_call(obj, "__isNullProto", &[], span);
                let none = self.mk(H::Lit(hir::Lit::Null), ret, span);
                let some = self.new_prototype(&p, ret, span);
                self.mk(
                    H::If {
                        cond: Box::new(test),
                        then: Box::new(none),
                        els: Box::new(some),
                    },
                    ret,
                    span,
                )
            }
            ProtoOf::Static(p) => self.new_prototype(&p, ret, span),
            ProtoOf::Classes(classes, own) => {
                let mut arms = vec![];
                for c in classes {
                    let Some(p) = self.class_proto(c) else {
                        continue;
                    };
                    let body = self.new_prototype(&p, ret, span);
                    arms.push(hir::Arm {
                        pat: self.pat(P::InstanceOf(c), t, span),
                        guard: None,
                        body,
                    });
                }
                let last = match own {
                    Some(p) => self.new_prototype(&p, ret, span),
                    // An interface value holds an instance of one of the classes.
                    None => match arms.pop() {
                        Some(arm) => arm.body,
                        None => self.mk(H::Lit(hir::Lit::Null), ret, span),
                    },
                };
                arms.push(hir::Arm {
                    pat: self.pat(P::Wildcard, t, span),
                    guard: None,
                    body: last,
                });
                let obj = self.mk(H::Local(l, hir::UseMode::Borrow), t, span);
                let kind = H::Match {
                    scrutinee: Box::new(obj),
                    arms,
                };
                self.mk(kind, ret, span)
            }
        }
    }

    /// `X.prototype.m` read as a value (not called through `.call`): not supported yet
    /// (`None`: `object` is not `X.prototype`).
    pub(crate) fn prototype_method_value(
        &mut self,
        object: &ast::Expr,
        prop: &ast::Ident,
        span: Span,
    ) -> Option<hir::Expr> {
        let ast::ExprKind::Member {
            object: o,
            prop: p,
            optional: false,
        } = &object.kind
        else {
            return None;
        };
        let ast::ExprKind::Ident(id) = &o.kind else {
            return None;
        };
        if p.name != "prototype" || self.is_local_name(&id.name) {
            return None;
        }
        let item = self.cx.lookup_item_at(self.module, &id.name, id.span);
        let known = match item {
            Some(Item::Def(d)) => self.cx.adt(d).is_some_and(|a| a.kind == AdtKind::Class),
            None => matches!(id.name.as_str(), "Array" | "Promise"),
            _ => false,
        };
        if !known {
            return None;
        }
        let what = format!("{}.prototype.{}", id.name, prop.name);
        let mut d = Diagnostic::error(format!("`{what}` as a value is not supported yet"), span);
        if id.name == "Object" && matches!(prop.name.as_str(), "hasOwnProperty" | "toString") {
            d = d.with_note(format!("call it directly: `{what}.call(o, ...)`"));
        }
        self.cx.error(d);
        Some(self.error_expr(span))
    }

    /// `X.prototype` as a value: `Object.prototype`, a class's, or a built-in collection's
    /// (`None`: `object` is not a name of one of them).
    pub(crate) fn prototype_member(&mut self, id: &ast::Ident, span: Span) -> Option<hir::Expr> {
        if self.is_local_name(&id.name) {
            return None;
        }
        let item = self.cx.lookup_item_at(self.module, &id.name, id.span);
        let proto = match item {
            Some(Item::Def(d)) if Some(d) == self.cx.prelude_adt("Object") => Some(Proto::object()),
            Some(Item::Def(d)) if self.cx.adt(d).is_some_and(|a| a.kind == AdtKind::Class) => {
                self.class_proto(d)
            }
            None if matches!(id.name.as_str(), "Array" | "Promise") => Proto::builtin(&id.name),
            _ => return None,
        };
        let Some(proto) = proto else {
            self.cx.err(
                format!("`{}.prototype` is not supported yet", id.name),
                span,
            );
            return Some(self.error_expr(span));
        };
        let ty = self.prototype_ty(span)?;
        Some(self.new_prototype(&proto, ty, span))
    }

    /// Are values of types `l` and `r` both prototypes (or `null`), which `===` compares by the
    /// prototype they name ([`Self::prototype_eq`])?
    pub(crate) fn prototype_operands(&self, l: TyId, r: TyId) -> bool {
        let Some(d) = self.cx.prelude_adt("ObjectPrototype") else {
            return false;
        };
        let is_proto = |t: TyId| {
            let t = self.cx.ty.opt_payload(t).unwrap_or(t);
            matches!(self.cx.ty.kind(t), TyKind::Adt(x, _) if *x == d)
        };
        is_proto(l) && is_proto(r)
    }

    /// `a === b` (`negate`: `!==`) of two prototypes ([`Self::prototype_operands`]): whether
    /// they name the same prototype.
    pub(crate) fn prototype_eq(
        &mut self,
        l: hir::Expr,
        r: hir::Expr,
        negate: bool,
        span: Span,
    ) -> hir::Expr {
        let found = self.cx.prelude_adt("ObjectPrototype").zip(
            match self.cx.prelude.get("__prototypeSame").cloned() {
                Some(Item::Def(f)) => Some(f),
                _ => None,
            },
        );
        let Some((d, f)) = found else {
            self.cx
                .err("prototypes need the prelude (std/prelude)", span);
            return self.error_expr(span);
        };
        let proto = self.cx.ty.intern(TyKind::Adt(d, vec![]));
        let opt = self.cx.ty.option(proto);
        let (l, r) = (self.coerce(l, opt), self.coerce(r, opt));
        let b = self.cx.ty.bool_;
        let call = self.mk(
            H::Call {
                callee: hir::Callee::Def(f, vec![]),
                args: vec![l, r],
            },
            b,
            span,
        );
        self.negated(call, negate, span)
    }
}
