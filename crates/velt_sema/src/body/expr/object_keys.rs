//! TypeScript's `Object.keys`, `Object.values` and `Object.entries` (the prelude's `Object`,
//! std/prelude/record.vlt), checked here rather than as ordinary generic calls:
//!
//! - `Object.keys(x)` accepts any object, as in TypeScript (issue #230), and returns a
//!   `string[]`: a record's keys in insertion order (`r.__keyNames()`), or the field names of an
//!   object type, struct or class instance in declaration order (base class fields first, as
//!   JS defines them). The names are known statically; only an optional field of a struct or
//!   object type (`b?: T`) is tested at run time, and listed while it is present, as
//!   `JSON.stringify` writes it: not null, or for `b?: T | null` its presence flag (a present
//!   `null` is listed). A class field is always listed, optional or not, as TS-compiled classes
//!   define every field.
//! - `Object.values(r)` / `Object.entries(r)` keep one value type, so they need a `Record`
//!   (an object literal is read as one, `record_literal.rs`).
//!
//! A value of a class with subclasses may hold a subclass instance, whose own fields JS lists
//! too: the names are chosen by the dynamic class, testing the subclasses deepest first
//! (`PatKind::InstanceOf`, a vtable read; `expr::downcast`). An interface value is tested the
//! same way against every class whose instances it can hold.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::setters::synth;
use crate::body::{FnCx, Want};
use crate::ctx::Item;
use crate::hir::{self, AdtKind, DefId, ExprKind as H, PatKind as P, TyId, TyKind};

/// What to use instead of `Object.values` / `Object.entries` on an object that is not a record.
const ONE_VALUE_TYPE_NOTE: &str = "they return one value type, so they need a `Record<K, V>`; list an object's keys with `Object.keys`, or keep values of different types in a `Record<string, V>` whose `V` is a union or `JsonValue`";

/// A key of an object type: its name, whether it is listed only while present, and whether its
/// presence is a flag (`b?: T | null`, `hir::FieldDef::presence`) rather than "not null".
struct Key {
    name: String,
    optional: bool,
    presence: bool,
}

impl FnCx<'_, '_> {
    /// `Object.keys(x)`, `Object.values(x)` or `Object.entries(x)` of the prelude's `Object`
    /// (`None`: another call, or an object literal for `values` / `entries`, which the generic
    /// signature reads as a record).
    pub(crate) fn object_helper_call(
        &mut self,
        callee: &ast::Expr,
        type_args: &[ast::TypeExpr],
        args: &[ast::Expr],
        span: Span,
    ) -> Option<hir::Expr> {
        let ast::ExprKind::Member {
            object,
            prop,
            optional: false,
        } = &callee.kind
        else {
            return None;
        };
        let (ast::ExprKind::Ident(o), [arg], true) = (&object.kind, args, type_args.is_empty())
        else {
            return None;
        };
        let keys = prop.name == "keys";
        if o.name != "Object" || self.is_local_name("Object") {
            return None;
        }
        if !keys && (!matches!(prop.name.as_str(), "values" | "entries") || is_object_lit(arg)) {
            return None;
        }
        let prelude = self.cx.prelude_adt("Object")?;
        match self.cx.lookup_item_at(self.module, "Object", o.span) {
            Some(Item::Def(d)) if d == prelude => {}
            _ => return None,
        }
        let obj = self.expr(arg, None, Want::Borrow);
        if self.cx.ty.is_bottom(obj.ty) {
            return Some(self.error_expr(span));
        }
        Some(if keys {
            self.object_keys(obj, span)
        } else {
            self.record_values(obj, &prop.name, span)
        })
    }

    /// `Object.values(r)` / `Object.entries(r)` (`helper`) of a checked value.
    fn record_values(&mut self, obj: hir::Expr, helper: &str, span: Span) -> hir::Expr {
        if self.record_args(obj.ty).is_some() {
            return self.record_call(obj, &format!("__{helper}"), &[], span);
        }
        let tn = self.cx.display(obj.ty);
        self.cx.error(
            Diagnostic::error(
                format!("`Object.{helper}` needs a `Record`, found `{tn}`"),
                obj.span,
            )
            .with_note(ONE_VALUE_TYPE_NOTE),
        );
        self.error_expr(span)
    }

    /// `Object.keys(obj)` of a checked value.
    fn object_keys(&mut self, obj: hir::Expr, span: Span) -> hir::Expr {
        if self.record_args(obj.ty).is_some() {
            return self.record_call(obj, "__keyNames", &[], span);
        }
        self.cx
            .object_copies
            .observe(obj.ty, span, crate::object_copies::Seen::Keys);
        // An interface value lists the fields of the class it holds (none can hold anything
        // else: see `iface_classes`).
        let (keys, classes) = match self.cx.ty.kind(obj.ty).clone() {
            TyKind::Dyn(i, _) => match self.iface_classes(i, obj.ty, obj.span) {
                Some(classes) => (vec![], Some(classes)),
                None => return self.error_expr(span),
            },
            _ => {
                let Some(keys) = self.object_key_names(obj.ty, obj.span) else {
                    return self.error_expr(span);
                };
                (keys, self.class_subclasses(obj.ty))
            }
        };
        let str_array = self.cx.ty.array(self.cx.ty.str_);
        let (l, mode) = self.option_binding(&obj, obj.ty, "<keys>", false);
        let names = if keys.iter().any(|k| k.optional) {
            self.present_keys(l, &keys, str_array, span)
        } else if let Some(classes) = classes {
            self.dynamic_class_keys(l, obj.ty, &keys, classes, span)
        } else {
            self.name_array(&keys, span)
        };
        // `obj` is evaluated once, as written, then the names follow from its type.
        let pat = self.pat(P::Binding(l, mode), obj.ty, span);
        let arms = vec![hir::Arm {
            pat,
            guard: None,
            body: names,
        }];
        let kind = H::Match {
            scrutinee: Box::new(obj),
            arms,
        };
        self.mk(kind, str_array, span)
    }

    /// `["a", "b"]`.
    fn name_array(&mut self, keys: &[Key], span: Span) -> hir::Expr {
        let lits = keys.iter().map(|k| self.str_lit(&k.name, span)).collect();
        let str_array = self.cx.ty.array(self.cx.ty.str_);
        self.mk(H::ArrayLit(lits), str_array, span)
    }

    /// The subclasses of class type `t`, deepest first (`None`: not a class, or no subclasses).
    pub(super) fn class_subclasses(&self, t: TyId) -> Option<Vec<DefId>> {
        let (d, _) = self.cx.class_of(t)?;
        let mut subs: Vec<(usize, DefId)> = (0..self.cx.info.len() as u32)
            .map(DefId)
            .filter(|&s| {
                s != d
                    && self.cx.adt(s).is_some_and(|a| a.kind == AdtKind::Class)
                    && self.cx.class_extends(s, d)
            })
            .map(|s| (self.class_depth(s), s))
            .collect();
        subs.sort_by(|a, b| b.0.cmp(&a.0).then(a.1 .0.cmp(&b.1 .0)));
        (!subs.is_empty()).then(|| subs.into_iter().map(|(_, s)| s).collect())
    }

    /// The classes whose instances an interface value of `iface` (type `t`) can hold, deepest
    /// first; `None` (reported at `span`) when a type that is not a class implements it, whose
    /// values carry no class to tell them apart.
    fn iface_classes(&mut self, iface: DefId, t: TyId, span: Span) -> Option<Vec<DefId>> {
        let mut classes = vec![];
        for d in (0..self.cx.info.len() as u32).map(DefId) {
            let Some(a) = self.cx.adt(d) else { continue };
            if self.cx.scopes[a.module].is_std {
                continue;
            }
            let (kind, name) = (a.kind, a.name.clone());
            if kind == AdtKind::Class {
                if self.holds_class(d, iface) {
                    classes.push((self.class_depth(d), d));
                }
                continue;
            }
            if self.implements(d, iface) {
                let tn = self.cx.display(t);
                self.cx.err(
                    format!("`Object.keys` cannot list the keys of a `{tn}`: `{name}` implements it and is not a class, so its values cannot be told apart at run time"),
                    span,
                );
                return None;
            }
        }
        classes.sort_by(|a, b| b.0.cmp(&a.0).then(a.1 .0.cmp(&b.1 .0)));
        Some(classes.into_iter().map(|(_, d)| d).collect())
    }

    /// May an interface value of `iface` hold an instance of class `d`: does `d`, a base class
    /// of it or one of its subclasses implement `iface`?
    fn holds_class(&mut self, d: DefId, iface: DefId) -> bool {
        if self.class_tree_implements(d, iface) {
            return true;
        }
        let mut cur = self.cx.adt(d).and_then(|a| a.base);
        while let Some((b, _)) = cur.and_then(|t| self.cx.class_of(t)) {
            if self.implements(b, iface) {
                return true;
            }
            cur = self.cx.adt(b).and_then(|a| a.base);
        }
        false
    }

    /// Does type definition `d` (over its own type parameters) implement `iface`?
    fn implements(&mut self, d: DefId, iface: DefId) -> bool {
        let n = self.cx.adt(d).map_or(0, |a| a.generics.len());
        let params: Vec<TyId> = (0..n as u32).map(|i| self.cx.ty.param(i)).collect();
        let t = self.cx.ty.intern(TyKind::Adt(d, params));
        self.cx.find_impl(t, iface).is_some()
    }

    /// The number of base classes of class `d`.
    fn class_depth(&self, d: DefId) -> usize {
        let mut cur = self.cx.adt(d).and_then(|a| a.base);
        let mut n = 0;
        while let Some((b, _)) = cur.and_then(|t| self.cx.class_of(t)) {
            n += 1;
            cur = self.cx.adt(b).and_then(|a| a.base);
        }
        n
    }

    /// The field names of the dynamic class of the object bound to `l` (static type `t`, whose
    /// own names are `keys`): `match (l) { InstanceOf(Sub) => [...], ..., _ => [...] }`.
    fn dynamic_class_keys(
        &mut self,
        l: hir::LocalId,
        t: TyId,
        keys: &[Key],
        subs: Vec<DefId>,
        span: Span,
    ) -> hir::Expr {
        let mut arms = vec![];
        for s in subs {
            let names: Vec<Key> = self.cx.adt(s).map_or(vec![], |a| {
                let fields = a.fields.iter().filter(|f| !is_private_name(&f.name));
                fields
                    .map(|f| Key {
                        name: f.name.clone(),
                        optional: false,
                        presence: false,
                    })
                    .collect()
            });
            arms.push(hir::Arm {
                pat: self.pat(P::InstanceOf(s), t, span),
                guard: None,
                body: self.name_array(&names, span),
            });
        }
        arms.push(hir::Arm {
            pat: self.pat(P::Wildcard, t, span),
            guard: None,
            body: self.name_array(keys, span),
        });
        let obj = self.mk(H::Local(l, hir::UseMode::Borrow), t, span);
        let str_array = self.cx.ty.array(self.cx.ty.str_);
        let kind = H::Match {
            scrutinee: Box::new(obj),
            arms,
        };
        self.mk(kind, str_array, span)
    }

    /// The keys of a value bound to `l`, testing the optional ones: `["a", ...(<keys>.b !=
    /// null ? ["b"] : [])]`, with a presence field's flag (`FieldPresent`, bound to a temporary
    /// first) instead of the null test.
    fn present_keys(
        &mut self,
        l: hir::LocalId,
        keys: &[Key],
        str_array: TyId,
        span: Span,
    ) -> hir::Expr {
        let name = format!("<keys@{}>", span.lo);
        let it = ast::Ident {
            name: name.clone(),
            span,
        };
        self.push_scope();
        if let Some(scope) = self.f.scopes.last_mut() {
            scope.names.insert(name, l);
        }
        let mut lets = vec![];
        let elems = keys
            .iter()
            .map(|k| {
                let lit = synth(ast::ExprKind::Lit(ast::Lit::Str(k.name.clone())), span);
                if !k.optional {
                    return lit;
                }
                let field = synth(
                    ast::ExprKind::Member {
                        object: Box::new(synth(ast::ExprKind::Ident(it.clone()), span)),
                        prop: ast::Ident {
                            name: k.name.clone(),
                            span,
                        },
                        optional: false,
                    },
                    span,
                );
                let present = match k.presence {
                    true => self.presence_flag(&field, &k.name, span, &mut lets),
                    false => synth(
                        ast::ExprKind::Binary {
                            op: ast::BinaryOp::NotEq,
                            lhs: Box::new(field),
                            rhs: Box::new(synth(ast::ExprKind::Lit(ast::Lit::Null), span)),
                        },
                        span,
                    ),
                };
                let cond = ast::ExprKind::Cond {
                    cond: Box::new(present),
                    then: Box::new(synth(ast::ExprKind::Array(vec![lit]), span)),
                    els: Box::new(synth(ast::ExprKind::Array(vec![]), span)),
                };
                synth(ast::ExprKind::Spread(Box::new(synth(cond, span))), span)
            })
            .collect();
        let array = synth(ast::ExprKind::Array(elems), span);
        let h = self.expr(&array, Some(str_array), Want::Move);
        self.pop_scope();
        self.with_lets(lets, h)
    }

    /// `FieldPresent(<field>)` bound to a temporary in the current scope, and the name that
    /// reads it.
    fn presence_flag(
        &mut self,
        field: &ast::Expr,
        key: &str,
        span: Span,
        lets: &mut Vec<hir::Stmt>,
    ) -> ast::Expr {
        let place = self.expr(field, None, Want::Borrow);
        let b = self.cx.ty.bool_;
        let flag = self.intrinsic(hir::Intrinsic::FieldPresent, vec![place], b, span);
        let name = format!("<present@{}:{key}>", span.lo);
        let read = self.temp(&name, flag, lets);
        if let (H::Local(local, _), Some(scope)) = (&read.kind, self.f.scopes.last_mut()) {
            scope.names.insert(name.clone(), *local);
        }
        synth(ast::ExprKind::Ident(ast::Ident { name, span }), span)
    }

    /// The keys `Object.keys` lists for a value of type `t` (its own static type's fields), or
    /// `None` (reported at `span`): `t` is not an object type, struct or class.
    fn object_key_names(&mut self, t: TyId, span: Span) -> Option<Vec<Key>> {
        let d = match self.cx.ty.kind(t) {
            TyKind::Adt(d, _) => Some(*d),
            _ => None,
        };
        let Some(a) = d.and_then(|d| self.cx.adt(d)) else {
            return self.not_an_object(t, span);
        };
        if a.kind != AdtKind::Anon && self.cx.scopes[a.module].is_std {
            return self.not_an_object(t, span);
        }
        let class = a.kind == AdtKind::Class;
        let keys = a
            .fields
            .iter()
            .filter(|f| !is_private_name(&f.name))
            .map(|f| Key {
                name: f.name.clone(),
                optional: f.optional && !class,
                presence: crate::anon::has_presence(&self.cx.ty, a.kind, f),
            });
        Some(keys.collect())
    }

    /// Reports that `Object.keys` cannot list the keys of a `t`.
    fn not_an_object<T>(&mut self, t: TyId, span: Span) -> Option<T> {
        let tn = self.cx.display(t);
        let mut diag = Diagnostic::error(
            format!(
                "`Object.keys` lists the keys of an object, a class or struct instance or a `Record`, found `{tn}`"
            ),
            span,
        );
        let map = self.cx.prelude_adt("Map");
        let fix = match self.cx.ty.kind(t) {
            TyKind::Adt(d, _) if Some(*d) == map => Some("list a map's keys with `m.keys()`"),
            TyKind::Adt(d, _) if self.cx.is_json_value(*d) => {
                Some("list a JSON object's keys with `v.keys()`")
            }
            TyKind::Option(_) => Some("test for `null` first"),
            _ => None,
        };
        if let Some(fix) = fix {
            diag = diag.with_note(fix);
        }
        self.cx.error(diag);
        None
    }
}

/// Is `e` an object literal (in parentheses or not)?
fn is_object_lit(e: &ast::Expr) -> bool {
    match &e.kind {
        ast::ExprKind::Paren(inner) => is_object_lit(inner),
        ast::ExprKind::Object(_) => true,
        _ => false,
    }
}

/// An ES private field (`#x`): `Object.keys` does not list it, as in JavaScript.
fn is_private_name(name: &str) -> bool {
    name.starts_with(ast::PRIVATE_NAME_PREFIX)
}
