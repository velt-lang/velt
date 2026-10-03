//! TypeScript's `Object.keys`, `Object.values` and `Object.entries` (the prelude's `Object`,
//! std/prelude/record.vlt), checked here rather than as ordinary generic calls:
//!
//! - `Object.keys(x)` accepts any object, as in TypeScript (issue #230), and returns a
//!   `string[]`: a record's keys in insertion order (`r.__keyNames()`), or the field names of an
//!   object type, struct or class instance in declaration order (base class fields first, as
//!   JS defines them). The names are known statically; only an optional field of a struct
//!   (`b?: T`) is tested at run time, and listed when it is not null, as `JSON.stringify` writes
//!   it. A class field is always listed, optional or not, as TS-compiled classes define every
//!   field. An object type's `b?: T` is the type `T | null` (sema keeps no separate optional
//!   flag), so its fields are always listed, as `JSON.stringify` and `console.log` show them.
//! - `Object.values(r)` / `Object.entries(r)` keep one value type, so they need a `Record`
//!   (an object literal is read as one, `record_literal.rs`).
//!
//! A class with subclasses is rejected: a value of its type may hold a subclass instance,
//! whose own fields JS lists too, and sema cannot test the dynamic class.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::setters::synth;
use crate::body::{FnCx, Want};
use crate::ctx::Item;
use crate::hir::{self, AdtKind, DefId, ExprKind as H, PatKind as P, TyId, TyKind};

/// What to use instead of `Object.values` / `Object.entries` on an object that is not a record.
const ONE_VALUE_TYPE_NOTE: &str = "they return one value type, so they need a `Record<K, V>`; list an object's keys with `Object.keys`, or keep values of different types in a `Record<string, V>` whose `V` is a union or `JsonValue`";

/// A key of an object type: its name, and whether it is listed only when not null.
struct Key {
    name: String,
    optional: bool,
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
        let Some(keys) = self.object_key_names(obj.ty, obj.span) else {
            return self.error_expr(span);
        };
        let str_array = self.cx.ty.array(self.cx.ty.str_);
        let (l, mode) = self.option_binding(&obj, obj.ty, "<keys>", false);
        let names = if keys.iter().any(|k| k.optional) {
            self.present_keys(l, &keys, str_array, span)
        } else {
            let lits = keys.iter().map(|k| self.str_lit(&k.name, span)).collect();
            self.mk(H::ArrayLit(lits), str_array, span)
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

    /// The keys of a value bound to `l`, testing the optional ones: `["a", ...(<keys>.b !=
    /// null ? ["b"] : [])]`.
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
                let present = synth(
                    ast::ExprKind::Binary {
                        op: ast::BinaryOp::NotEq,
                        lhs: Box::new(field),
                        rhs: Box::new(synth(ast::ExprKind::Lit(ast::Lit::Null), span)),
                    },
                    span,
                );
                let cond = ast::ExprKind::Cond {
                    cond: Box::new(present),
                    then: Box::new(synth(ast::ExprKind::Array(vec![lit]), span)),
                    els: Box::new(synth(ast::ExprKind::Array(vec![]), span)),
                };
                synth(ast::ExprKind::Spread(Box::new(synth(cond, span))), span)
            })
            .collect();
        let array = synth(ast::ExprKind::Array(elems), span);
        self.push_scope();
        if let Some(scope) = self.f.scopes.last_mut() {
            scope.names.insert(name, l);
        }
        let h = self.expr(&array, Some(str_array), Want::Move);
        self.pop_scope();
        h
    }

    /// The keys `Object.keys` lists for a value of type `t`, or `None` (reported at `span`):
    /// `t` is not an object type, struct or class, or is a class with subclasses.
    fn object_key_names(&mut self, t: TyId, span: Span) -> Option<Vec<Key>> {
        let d = match self.cx.ty.kind(t) {
            TyKind::Adt(d, _) => Some(*d),
            _ => None,
        };
        let Some((d, a)) = d.and_then(|d| self.cx.adt(d).map(|a| (d, a))) else {
            return self.not_an_object(t, span);
        };
        if a.kind != AdtKind::Anon && self.cx.scopes[a.module].is_std {
            return self.not_an_object(t, span);
        }
        let class = a.kind == AdtKind::Class;
        let keys = a
            .fields
            .iter()
            .map(|f| Key {
                name: f.name.clone(),
                optional: f.optional && !class,
            })
            .collect();
        if class {
            if let Some(sub) = self.subclass_of(d) {
                let (tn, sn) = (self.cx.display(t), self.cx.adt(sub).map(|s| s.name.clone()));
                let sn = sn.unwrap_or_default();
                self.cx.error(
                    Diagnostic::error(
                        format!("`Object.keys` cannot list the fields of `{tn}`: it has subclasses"),
                        span,
                    )
                    .with_note(format!(
                        "a `{tn}` value may be a `{sn}`, whose own fields would be listed too; call it on a value of a class without subclasses"
                    )),
                );
                return None;
            }
        }
        Some(keys)
    }

    /// A class that extends class `d` (directly or not), if any.
    fn subclass_of(&self, d: DefId) -> Option<DefId> {
        (0..self.cx.info.len() as u32).map(DefId).find(|&s| {
            s != d
                && self.cx.adt(s).is_some_and(|a| a.kind == AdtKind::Class)
                && self.cx.class_extends(s, d)
        })
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
