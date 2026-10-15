//! JavaScript's `Object` built-ins beyond `keys` / `values` / `entries` (object_keys.rs), and
//! `Array.isArray` (docs/reference/std/object.md). Each is checked here against the static types
//! of its arguments and becomes ordinary code over them; a program that doesn't call them gets
//! none of it.
//!
//! - `Object.is(a, b)`: JavaScript's SameValue (`Intrinsic::SameValue`): `===`, except that `NaN`
//!   is itself and `0` is not `-0`. Values of unrelated types are never the same.
//! - `Array.isArray(x)`: a test of the array members of `x`'s type, which narrows `x` too
//!   (`body::narrow`); a `JsonValue`'s `isArray()`.
//! - `Object.assign(target, ...sources)`: each source's own fields written to the target, in
//!   order, through its setters; the target itself is the result. A record target takes any
//!   keys; an object or class target only the fields its type declares.
//! - `Object.freeze(o)` of a fresh object literal: the object, typed with every field readonly.
//!   Its type is a definition of its own (`Ctx::frozen_defs`) that doesn't convert to a type
//!   whose fields can be written, so no write reaches it (Node throws a `TypeError` on one).
//! - `Object.fromEntries(pairs)`: a `Record<string, V>` (std/prelude/record.vlt); an iterable
//!   that is not an array goes through `Array.from` first.
//! - `Object.hasOwn(o, k)` and `Object.prototype.hasOwnProperty.call(o, k)`: a record's entry
//!   test, or for other objects whether `Object.keys(o)` lists `k`.
//! - `Object.prototype.toString.call(v)`: the `[object Tag]` string of each member type.
//! - `Object.getPrototypeOf(v)`, `Object.prototype`, `C.prototype`: an `ObjectPrototype` value
//!   naming the prototype (std/prelude/record.vlt), which `===` compares by what it names
//!   (`__prototypeSame`, from `binary`).
//!
//! What Velt's fixed object layouts can't do exactly is an error saying it is not supported yet,
//! never a silent difference: other `Object` functions, prototypes of built-in values whose
//! behaviour as objects Velt doesn't model (`Date.prototype`, `String.prototype`), and adding
//! fields a type doesn't declare. The operations are each one function taking checked operands,
//! so that a `Proxy` (#823) can add its traps in one place.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::setters::synth;
use crate::body::{FnCx, Want};
use crate::ctx::Item;
use crate::hir::{self, AdtKind, DefId, ExprKind as H, Intrinsic, PatKind as P, TyId, TyKind};

/// The functions of JavaScript's `Object` that Velt doesn't have yet (`keys`, `values`,
/// `entries` and the ones here are handled).
const NOT_YET: [&str; 15] = [
    "defineProperty",
    "defineProperties",
    "getOwnPropertyNames",
    "getOwnPropertyDescriptor",
    "getOwnPropertyDescriptors",
    "getOwnPropertySymbols",
    "isFrozen",
    "isSealed",
    "isExtensible",
    "seal",
    "preventExtensions",
    "setPrototypeOf",
    "groupBy",
    "fromAsync",
    "hasOwnProperty",
];

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
    /// A call of one of the built-ins this module handles (`None`: another call).
    pub(crate) fn object_builtin_call(
        &mut self,
        callee: &ast::Expr,
        type_args: &[ast::TypeExpr],
        args: &[ast::Expr],
        exp: Option<TyId>,
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
        if prop.name == "call" {
            if let Some(m) = self.object_prototype_method(object) {
                return Some(self.prototype_method_call(&m, args, span));
            }
        }
        let ast::ExprKind::Ident(ns) = &object.kind else {
            return None;
        };
        if ns.name == "Array" && prop.name == "isArray" && self.builtin_array(ns) {
            return Some(self.array_is_array(args, span));
        }
        if !self.is_prelude_object(ns) {
            return None;
        }
        let name = prop.name.as_str();
        let known = matches!(
            name,
            "is" | "assign" | "freeze" | "fromEntries" | "hasOwn" | "getPrototypeOf" | "create"
        );
        if !known && !NOT_YET.contains(&name) {
            return None;
        }
        if !type_args.is_empty() && name != "fromEntries" {
            self.cx
                .err(format!("`Object.{name}` takes no type arguments"), span);
        }
        Some(match name {
            "is" => self.object_is(args, span),
            "assign" => self.object_assign(args, span),
            "freeze" => self.object_freeze(args, span),
            "fromEntries" => self.object_from_entries(callee, type_args, args, exp, span)?,
            "hasOwn" => self.has_own(args, "Object.hasOwn", span),
            "getPrototypeOf" => self.get_prototype_of(args, span),
            "create" => self.object_create(args, exp, span),
            _ => {
                let mut d =
                    Diagnostic::error(format!("`Object.{name}` is not supported yet"), prop.span);
                if name == "defineProperty" || name == "defineProperties" {
                    d = d.with_note("Velt objects have the fields their type declares (#922)");
                }
                self.cx.error(d);
                self.check_args_loose(args);
                self.error_expr(span)
            }
        })
    }

    /// Does `ns` name the prelude's `Object` (not shadowed)?
    pub(crate) fn is_prelude_object(&mut self, ns: &ast::Ident) -> bool {
        if ns.name != "Object" || self.is_local_name("Object") {
            return false;
        }
        let prelude = self.cx.prelude_adt("Object");
        matches!(
            self.cx.lookup_item_at(self.module, "Object", ns.span),
            Some(Item::Def(d)) if Some(d) == prelude
        )
    }

    /// Does `ns` name the built-in `Array` (no local, function or class of that name)?
    fn builtin_array(&mut self, ns: &ast::Ident) -> bool {
        ns.name == "Array"
            && !self.is_local_name("Array")
            && self
                .cx
                .lookup_item_at(self.module, "Array", ns.span)
                .is_none()
    }

    /// Is `callee` `Array.isArray` (the built-in), and `args` its one argument?
    pub(crate) fn is_array_test<'a>(
        &mut self,
        callee: &ast::Expr,
        args: &'a [ast::Expr],
    ) -> Option<&'a ast::Expr> {
        let ast::ExprKind::Member {
            object,
            prop,
            optional: false,
        } = &callee.kind
        else {
            return None;
        };
        let (ast::ExprKind::Ident(ns), [arg]) = (&object.kind, args) else {
            return None;
        };
        (prop.name == "isArray" && self.builtin_array(ns)).then_some(arg)
    }

    /// Wrong number of arguments for `what` (TypeScript's TS2554).
    fn arity_error(
        &mut self,
        what: &str,
        want: usize,
        args: &[ast::Expr],
        span: Span,
    ) -> hir::Expr {
        self.cx.err(
            format!(
                "`{what}` expects {want} argument(s), but got {} (TypeScript's TS2554)",
                args.len()
            ),
            span,
        );
        self.check_args_loose(args);
        self.error_expr(span)
    }

    /// `Array.isArray(x)`.
    fn array_is_array(&mut self, args: &[ast::Expr], span: Span) -> hir::Expr {
        let [arg] = args else {
            return self.arity_error("Array.isArray", 1, args, span);
        };
        let json = |s: &Self, t: TyId| matches!(s.cx.ty.kind(t), TyKind::Adt(d, _) if s.cx.is_json_value(*d));
        if let Some(t) = self.trial_type(arg) {
            let nullable = self.cx.ty.opt_payload(t);
            if json(self, nullable.unwrap_or(t)) {
                // `v.isArray()`, or `v?.isArray() ?? false` for a `JsonValue | null`.
                let call = synth(
                    ast::ExprKind::Call {
                        callee: Box::new(synth(
                            ast::ExprKind::Member {
                                object: Box::new(arg.clone()),
                                prop: ast::Ident {
                                    name: "isArray".to_string(),
                                    span,
                                },
                                optional: nullable.is_some(),
                            },
                            span,
                        )),
                        type_args: vec![],
                        args: vec![],
                        optional: false,
                    },
                    span,
                );
                let e = match nullable {
                    None => call,
                    Some(_) => synth(
                        ast::ExprKind::Binary {
                            op: ast::BinaryOp::Nullish,
                            lhs: Box::new(call),
                            rhs: Box::new(synth(ast::ExprKind::Lit(ast::Lit::Bool(false)), span)),
                        },
                        span,
                    ),
                };
                return self.expr(&e, None, Want::Move);
            }
            let members = self
                .cx
                .union_members(nullable.unwrap_or(t))
                .unwrap_or_default();
            if members.iter().any(|m| json(self, *m)) {
                let tn = self.cx.display(t);
                self.cx.err(
                    format!("`Array.isArray` of a `{tn}` is not supported yet: test the `JsonValue` member with `isArray()`"),
                    arg.span,
                );
                return self.error_expr(span);
            }
        }
        let s = self.expr(arg, None, Want::Borrow);
        if self.cx.ty.is_bottom(s.ty) {
            return self.error_expr(span);
        }
        let pred = |cx: &crate::ctx::Ctx, t: TyId| matches!(cx.ty.kind(t), TyKind::Array(_));
        self.type_test(s, &pred, false, span).expr
    }

    /// `Object.is(a, b)`.
    fn object_is(&mut self, args: &[ast::Expr], span: Span) -> hir::Expr {
        let [a, b] = args else {
            return self.arity_error("Object.is", 2, args, span);
        };
        // Against `null` or `undefined`, SameValue is `===`.
        let nothing = |e: &ast::Expr| {
            matches!(&strip_parens(e).kind, ast::ExprKind::Lit(ast::Lit::Null))
                || matches!(&strip_parens(e).kind, ast::ExprKind::Ident(i) if i.name == "undefined")
        };
        if nothing(a) && nothing(b) {
            // `Object.is(null, null)`: `null` is itself, and not `undefined`.
            let same = matches!(
                (&strip_parens(a).kind, &strip_parens(b).kind),
                (ast::ExprKind::Lit(_), ast::ExprKind::Lit(_))
                    | (ast::ExprKind::Ident(_), ast::ExprKind::Ident(_))
            );
            let b = self.cx.ty.bool_;
            return self.mk(H::Lit(hir::Lit::Bool(same)), b, span);
        }
        if nothing(a) || nothing(b) {
            let eq = synth(
                ast::ExprKind::Binary {
                    op: ast::BinaryOp::StrictEq,
                    lhs: Box::new(a.clone()),
                    rhs: Box::new(b.clone()),
                },
                span,
            );
            return self.expr(&eq, None, Want::Move);
        }
        let (l, r) = self.operands(a, b, None, Want::Borrow);
        if self.cx.ty.is_bottom(l.ty) || self.cx.ty.is_bottom(r.ty) {
            return self.error_expr(span);
        }
        let (l, r) = (self.unbrand(l), self.unbrand(r));
        let (l, r) = self.nullable_operands(l, r);
        let (l, r) = self.union_operands(l, r);
        let (l, r) = self.identity_operands(l, r);
        let (l, r) = if l.ty != r.ty || self.cx.lit_value(l.ty).is_some() {
            (self.widen_value(l), self.widen_value(r))
        } else {
            (l, r)
        };
        let (l, r) = self.mix_numbers(l, r);
        let b = self.cx.ty.bool_;
        if l.ty != r.ty {
            // Values of unrelated types are never the same value: both are evaluated, in order.
            let stmts = [l, r]
                .into_iter()
                .map(|e| hir::Stmt {
                    span: e.span,
                    kind: hir::StmtKind::Expr(e),
                })
                .collect();
            let no = self.mk(H::Lit(hir::Lit::Bool(false)), b, span);
            let block = hir::Block {
                stmts,
                value: Some(Box::new(no)),
                span,
            };
            return self.mk(H::Block(block), b, span);
        }
        self.intrinsic(Intrinsic::SameValue, vec![l, r], b, span)
    }

    /// `Object.assign(target, ...sources)`.
    fn object_assign(&mut self, args: &[ast::Expr], span: Span) -> hir::Expr {
        let Some((target, sources)) = args.split_first() else {
            return self.arity_error("Object.assign", 1, args, span);
        };
        let t = self.expr(target, None, Want::Borrow);
        if self.cx.ty.is_bottom(t.ty) {
            self.check_args_loose(sources);
            return self.error_expr(span);
        }
        let tty = t.ty;
        let record = self.record_args(tty).is_some();
        let fields = match record {
            true => None,
            false => match self.assign_target_fields(tty, target.span) {
                Some(f) => Some(f),
                None => {
                    self.check_args_loose(sources);
                    return self.error_expr(span);
                }
            },
        };
        self.push_scope();
        let mut lets = vec![];
        let tname = format!("<assign@{}>", span.lo);
        let t_read = self.named_temp(&tname, t, &mut lets);
        for (i, src) in sources.iter().enumerate() {
            let exp = record.then_some(tty);
            let s = self.expr(src, exp, Want::Borrow);
            if self.cx.ty.is_bottom(s.ty) {
                continue;
            }
            let sty = s.ty;
            let sname = format!("<assign@{}:{i}>", span.lo);
            if record && self.record_args(sty).is_some() {
                let s = self.coerce(s, tty);
                let s_read = self.named_temp(&sname, s, &mut lets);
                let tr = t_read.clone();
                let ext = self.record_entries_into(tr, s_read, span);
                lets.push(hir::Stmt {
                    kind: hir::StmtKind::Expr(ext),
                    span,
                });
                continue;
            }
            let Some(keys) = self.assign_source_keys(sty, src.span) else {
                continue;
            };
            self.named_temp(&sname, s, &mut lets);
            for (key, optional) in keys {
                if let Some(fields) = &fields {
                    if !fields.contains(&key) {
                        let tn = self.cx.display(tty);
                        self.cx.error(
                            Diagnostic::error(
                                format!("`Object.assign` cannot add `{key}` to a `{tn}`: adding a field its type doesn't declare is not supported yet"),
                                src.span,
                            )
                            .with_note(format!("Velt objects have the fields their type declares; declare `{key}` in the target's type (`{key}?: T` if it may be absent)")),
                        );
                        continue;
                    }
                }
                let read = |name: &str| {
                    synth(
                        ast::ExprKind::Ident(ast::Ident {
                            name: name.to_string(),
                            span,
                        }),
                        span,
                    )
                };
                let field = synth(
                    ast::ExprKind::Member {
                        object: Box::new(read(&sname)),
                        prop: ast::Ident {
                            name: key.clone(),
                            span,
                        },
                        optional: false,
                    },
                    span,
                );
                let place = match record {
                    true => ast::ExprKind::Index {
                        object: Box::new(read(&tname)),
                        index: Box::new(synth(
                            ast::ExprKind::Lit(ast::Lit::Str(key.clone())),
                            span,
                        )),
                        optional: false,
                    },
                    false => ast::ExprKind::Member {
                        object: Box::new(read(&tname)),
                        prop: ast::Ident {
                            name: key.clone(),
                            span,
                        },
                        optional: false,
                    },
                };
                let assign = synth(
                    ast::ExprKind::Assign {
                        op: None,
                        target: Box::new(synth(place, span)),
                        value: Box::new(field.clone()),
                    },
                    span,
                );
                // An optional field that is absent is not an own property: not copied.
                let stmt = match optional {
                    false => assign,
                    true => synth(
                        ast::ExprKind::Cond {
                            cond: Box::new(synth(
                                ast::ExprKind::Binary {
                                    op: ast::BinaryOp::NotEq,
                                    lhs: Box::new(field),
                                    rhs: Box::new(synth(ast::ExprKind::Lit(ast::Lit::Null), span)),
                                },
                                span,
                            )),
                            then: Box::new(synth(ast::ExprKind::Paren(Box::new(assign)), span)),
                            els: Box::new(read(&tname)),
                        },
                        span,
                    ),
                };
                let h = self.expr(&stmt, None, Want::Move);
                lets.push(hir::Stmt {
                    span,
                    kind: hir::StmtKind::Expr(h),
                });
            }
        }
        self.pop_scope();
        self.with_lets(lets, t_read)
    }

    /// A temporary holding `init`, which synthesized code reads by `name` (in the current
    /// scope).
    fn named_temp(&mut self, name: &str, init: hir::Expr, lets: &mut Vec<hir::Stmt>) -> hir::Expr {
        let read = self.temp(name, init, lets);
        if let (H::Local(l, _), Some(scope)) = (&read.kind, self.f.scopes.last_mut()) {
            scope.names.insert(name.to_string(), *l);
        }
        read
    }

    /// `target.__extend(source)` of two records.
    fn record_entries_into(
        &mut self,
        target: hir::Expr,
        source: hir::Expr,
        span: Span,
    ) -> hir::Expr {
        let arg = match &source.kind {
            H::Local(l, _) => {
                let name = self
                    .f
                    .scopes
                    .iter()
                    .rev()
                    .find_map(|s| {
                        s.names
                            .iter()
                            .find(|(_, v)| *v == l)
                            .map(|(k, _)| k.clone())
                    })
                    .unwrap_or_default();
                synth(ast::ExprKind::Ident(ast::Ident { name, span }), span)
            }
            _ => unreachable!("ICE: record source is a temporary"),
        };
        self.record_call(target, "__extend", &[arg], span)
    }

    /// The fields an `Object.assign` target of type `t` (not a record) can be given, or `None`
    /// (reported at `span`): it is not an object or class instance.
    fn assign_target_fields(&mut self, t: TyId, span: Span) -> Option<Vec<String>> {
        let what = match self.cx.ty.kind(t) {
            TyKind::Adt(d, _) => match self.cx.adt(*d) {
                Some(a) if a.kind == AdtKind::Anon => None,
                Some(a) if a.kind == AdtKind::Class && !self.cx.scopes[a.module].is_std => None,
                _ => Some("a value of a built-in type"),
            },
            TyKind::Closure(_) | TyKind::FnPtr { .. } => Some("a function (#921)"),
            TyKind::Array(_) => Some("an array"),
            _ => Some("a primitive or interface value"),
        };
        if let Some(what) = what {
            let tn = self.cx.display(t);
            self.cx.err(
                format!("`Object.assign` onto {what} (`{tn}`) is not supported yet: the target must be an object, a class instance or a `Record`"),
                span,
            );
            return None;
        }
        let TyKind::Adt(d, _) = self.cx.ty.kind(t) else {
            return None;
        };
        let a = self.cx.adt(*d)?;
        let mut names: Vec<String> = a.fields.iter().map(|f| f.name.clone()).collect();
        // Accessors defined by the class (a setter is how JavaScript's assignment reaches them).
        names.extend(
            a.methods
                .keys()
                .filter_map(|m| m.strip_prefix("set ").map(str::to_string)),
        );
        Some(names)
    }

    /// The own keys an `Object.assign` source of type `t` gives, each with whether it is an
    /// optional field (copied only while present), or `None` (reported at `span`).
    fn assign_source_keys(&mut self, t: TyId, span: Span) -> Option<Vec<(String, bool)>> {
        if self.class_subclasses(t).is_some() || matches!(self.cx.ty.kind(t), TyKind::Dyn(..)) {
            let tn = self.cx.display(t);
            self.cx.err(
                format!("`Object.assign` from a `{tn}`, which may hold an instance of another class, is not supported yet"),
                span,
            );
            return None;
        }
        let keys = self.object_key_names(t, span)?;
        let a = match self.cx.ty.kind(t) {
            TyKind::Adt(d, _) => self.cx.adt(*d),
            _ => None,
        }?;
        let class = a.kind == AdtKind::Class;
        let mut out = vec![];
        for k in keys {
            let f = a.fields.iter().find(|f| f.name == k.name)?;
            if k.optional && crate::anon::has_presence(&self.cx.ty, a.kind, f) {
                let tn = self.cx.display(t);
                self.cx.err(
                    format!("`Object.assign` from a `{tn}`, whose optional field `{}` may hold `null`, is not supported yet", k.name),
                    span,
                );
                return None;
            }
            out.push((k.name, k.optional && !class));
        }
        Some(out)
    }

    /// `Object.freeze(o)`.
    fn object_freeze(&mut self, args: &[ast::Expr], span: Span) -> hir::Expr {
        let [arg] = args else {
            return self.arity_error("Object.freeze", 1, args, span);
        };
        let fresh = matches!(strip_parens(arg).kind, ast::ExprKind::Object(_));
        let h = self.expr(arg, None, Want::Move);
        if self.cx.ty.is_bottom(h.ty) {
            return self.error_expr(span);
        }
        let frozen = fresh
            .then(|| self.cx.frozen_type(h.ty, self.module))
            .flatten();
        let Some(frozen) = frozen else {
            let tn = self.cx.display(h.ty);
            self.cx.error(
                Diagnostic::error(
                    format!("`Object.freeze` of a `{tn}` is not supported yet"),
                    arg.span,
                )
                .with_note("Velt freezes a new object literal, `Object.freeze({ ... })`, whose fields are then readonly; for other values, use a `Readonly<T>` type"),
            );
            return self.error_expr(span);
        };
        self.coerce(h, frozen)
    }

    /// `Object.fromEntries(pairs)`: the prelude's, with an iterable that is not an array
    /// passed through `Array.from` (`None`: the prelude call as written).
    fn object_from_entries(
        &mut self,
        callee: &ast::Expr,
        type_args: &[ast::TypeExpr],
        args: &[ast::Expr],
        exp: Option<TyId>,
        span: Span,
    ) -> Option<hir::Expr> {
        let [arg] = args else {
            return None;
        };
        if matches!(strip_parens(arg).kind, ast::ExprKind::Array(_)) {
            return None;
        }
        let ty = self.trial_type(arg);
        if ty.is_none_or(|t| {
            matches!(self.cx.ty.kind(t), TyKind::Array(_)) || self.cx.ty.is_bottom(t)
        }) {
            return None;
        }
        // `Array.from(pairs)`: a `Map`'s entries, a generator's values.
        let from = synth(
            ast::ExprKind::Call {
                callee: Box::new(synth(
                    ast::ExprKind::Member {
                        object: Box::new(synth(
                            ast::ExprKind::Ident(ast::Ident {
                                name: "Array".to_string(),
                                span: arg.span,
                            }),
                            arg.span,
                        )),
                        prop: ast::Ident {
                            name: "from".to_string(),
                            span: arg.span,
                        },
                        optional: false,
                    },
                    arg.span,
                )),
                type_args: vec![],
                args: vec![arg.clone()],
                optional: false,
            },
            arg.span,
        );
        let ast::ExprKind::Member { object, prop, .. } = &callee.kind else {
            return None;
        };
        Some(self.member_call(object, prop, false, type_args, &[from], exp, span))
    }

    /// The type `e` checks to, without keeping anything of the check (`None`: it has errors).
    pub(super) fn trial_type(&mut self, e: &ast::Expr) -> Option<TyId> {
        let mark = crate::body::recheck::Mark::here(self.cx);
        let frames = self.trial_frames();
        let diags = self.cx.diags.len();
        let temps = self.call_temps.len();
        let ty = self.expr(e, None, Want::Borrow).ty;
        let failed = self.cx.diags.len() > diags;
        mark.rollback(self.cx);
        self.cx.diags.truncate(diags);
        self.call_temps.truncate(temps);
        self.restore_trial_frames(frames);
        (!failed).then_some(ty)
    }

    /// `Object.prototype.m` (the prelude's `Object`), as the callee of `.call`: `m`.
    fn object_prototype_method(&mut self, e: &ast::Expr) -> Option<String> {
        let ast::ExprKind::Member {
            object,
            prop,
            optional: false,
        } = &e.kind
        else {
            return None;
        };
        let ast::ExprKind::Member {
            object: o,
            prop: p,
            optional: false,
        } = &object.kind
        else {
            return None;
        };
        let ast::ExprKind::Ident(ns) = &o.kind else {
            return None;
        };
        (p.name == "prototype" && self.is_prelude_object(ns)).then(|| prop.name.clone())
    }

    /// `Object.prototype.m.call(args)`.
    fn prototype_method_call(&mut self, m: &str, args: &[ast::Expr], span: Span) -> hir::Expr {
        match m {
            "hasOwnProperty" => self.has_own(args, "Object.prototype.hasOwnProperty.call", span),
            "toString" => self.object_to_string_call(args, span),
            _ => {
                self.cx.err(
                    format!("`Object.prototype.{m}.call` is not supported yet"),
                    span,
                );
                self.check_args_loose(args);
                self.error_expr(span)
            }
        }
    }

    /// `Object.hasOwn(o, k)` / `Object.prototype.hasOwnProperty.call(o, k)` (`what`).
    fn has_own(&mut self, args: &[ast::Expr], what: &str, span: Span) -> hir::Expr {
        let [o, k] = args else {
            return self.arity_error(what, 2, args, span);
        };
        let Some(ot) = self.trial_type(o) else {
            self.check_args_loose(args);
            return self.error_expr(span);
        };
        if let Some((key, _)) = self.record_args(ot) {
            if key == self.cx.ty.str_ {
                let obj = self.expr(o, None, Want::Borrow);
                return self.record_call(obj, "__has", std::slice::from_ref(k), span);
            }
        }
        let object = match self.cx.ty.kind(ot) {
            TyKind::Adt(d, _) => self
                .cx
                .adt(*d)
                .is_some_and(|a| !self.cx.scopes[a.module].is_std || a.kind == AdtKind::Anon),
            TyKind::Dyn(..) => true,
            _ => false,
        };
        if !object {
            let tn = self.cx.display(ot);
            let mut d = Diagnostic::error(
                format!("`{what}` on a `{tn}` is not supported yet: it tests the own keys of an object, a class instance or a `Record<string, V>`"),
                o.span,
            );
            if self.cx.ty.opt_payload(ot).is_some() {
                d = d.with_note("test for `null` first (Node throws a `TypeError` for `null`)");
            }
            self.cx.error(d);
            self.check_args_loose(args);
            return self.error_expr(span);
        }
        if self
            .trial_type(k)
            .is_some_and(|t| matches!(self.cx.ty.kind(t), TyKind::Symbol))
        {
            self.cx.err(
                format!("`{what}` with a symbol key is not supported yet"),
                k.span,
            );
            self.check_args_loose(args);
            return self.error_expr(span);
        }
        // `Object.keys(o).includes(k)`: the built-in `Object.keys` (an empty callee span, as a
        // `for...in` loop writes it), which knows every object's own string keys.
        let empty = Span::new(span.file, span.lo, span.lo);
        let keys = synth(
            ast::ExprKind::Call {
                callee: Box::new(synth(
                    ast::ExprKind::Member {
                        object: Box::new(synth(
                            ast::ExprKind::Ident(ast::Ident {
                                name: "Object".to_string(),
                                span: empty,
                            }),
                            empty,
                        )),
                        prop: ast::Ident {
                            name: "keys".to_string(),
                            span: empty,
                        },
                        optional: false,
                    },
                    empty,
                )),
                type_args: vec![],
                args: vec![o.clone()],
                optional: false,
            },
            o.span,
        );
        let call = synth(
            ast::ExprKind::Call {
                callee: Box::new(synth(
                    ast::ExprKind::Member {
                        object: Box::new(keys),
                        prop: ast::Ident {
                            name: "includes".to_string(),
                            span,
                        },
                        optional: false,
                    },
                    span,
                )),
                type_args: vec![],
                args: vec![k.clone()],
                optional: false,
            },
            span,
        );
        self.expr(&call, None, Want::Move)
    }

    /// `Object.prototype.toString.call(v)`: `"[object Tag]"`.
    fn object_to_string_call(&mut self, args: &[ast::Expr], span: Span) -> hir::Expr {
        let [v] = args else {
            return self.arity_error("Object.prototype.toString.call", 1, args, span);
        };
        let s = self.expr(v, None, Want::Borrow);
        if self.cx.ty.is_bottom(s.ty) {
            return self.error_expr(span);
        }
        let sty = s.ty;
        let mut arms = vec![];
        for (pat, t) in self.member_patterns(sty, span) {
            let tag = match t {
                None => match self.cx.ty.nothing_of(sty) {
                    Some(hir::Nothing::Undefined) => Some("Undefined".to_string()),
                    _ => Some("Null".to_string()),
                },
                Some(t) => self.string_tag(t),
            };
            let Some(tag) = tag else {
                let tn = self.cx.display(t.unwrap_or(sty));
                self.cx.err(
                    format!("`Object.prototype.toString.call` of a `{tn}` is not supported yet"),
                    v.span,
                );
                return self.error_expr(span);
            };
            let body = self.str_lit(&format!("[object {tag}]"), span);
            arms.push(hir::Arm {
                pat,
                guard: None,
                body,
            });
        }
        let str_ = self.cx.ty.str_;
        let kind = H::Match {
            scrutinee: Box::new(s),
            arms,
        };
        self.mk(kind, str_, span)
    }

    /// The `Symbol.toStringTag` JavaScript gives values of type `t` (`None`: not known).
    fn string_tag(&mut self, t: TyId) -> Option<String> {
        let ty = &self.cx.ty;
        let tag = match ty.kind(t) {
            TyKind::Array(_) | TyKind::Tuple(_) => "Array",
            TyKind::Str | TyKind::Literal(hir::LitValue::Str(_)) => "String",
            TyKind::Bool | TyKind::Literal(hir::LitValue::Bool(_)) => "Boolean",
            TyKind::Int(_) | TyKind::Float(_) | TyKind::Literal(_) => "Number",
            TyKind::Closure(_) | TyKind::FnPtr { .. } => "Function",
            TyKind::Promise(..) => "Promise",
            TyKind::Adt(d, _) => {
                let d = *d;
                if self.cx.union_def(t).is_some() {
                    return None;
                }
                let a = self.cx.adt(d)?;
                if a.kind == AdtKind::Anon || self.record_args(t).is_some() {
                    return Some("Object".to_string());
                }
                // The nearest class (itself or a base) that is the standard library's.
                let mut cur = Some(d);
                while let Some(c) = cur {
                    let a = self.cx.adt(c)?;
                    if a.methods.contains_key("[Symbol.toStringTag]")
                        || a.methods.contains_key("get [Symbol.toStringTag]")
                        || a.fields.iter().any(|f| f.name == "[Symbol.toStringTag]")
                    {
                        return None;
                    }
                    if self.cx.scopes[a.module].is_std {
                        let name = a.name.clone();
                        return std_string_tag(self, c, &name).map(str::to_string);
                    }
                    cur = a.base.and_then(|b| self.cx.class_of(b)).map(|(b, _)| b);
                }
                "Object"
            }
            _ => return None,
        };
        Some(tag.to_string())
    }

    /// `Object.getPrototypeOf(v)`.
    fn get_prototype_of(&mut self, args: &[ast::Expr], span: Span) -> hir::Expr {
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
            self.cx.err("prototypes need the prelude (std/prelude)", span);
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

    /// `Object.create(proto)`: `Object.create(null)` where a `Record<string, V>` is expected.
    fn object_create(&mut self, args: &[ast::Expr], exp: Option<TyId>, span: Span) -> hir::Expr {
        let [proto] = args else {
            if args.len() == 2 {
                self.cx.err(
                    "`Object.create` with property descriptors is not supported yet",
                    args[1].span,
                );
                self.check_args_loose(args);
                return self.error_expr(span);
            }
            return self.arity_error("Object.create", 1, args, span);
        };
        if !matches!(strip_parens(proto).kind, ast::ExprKind::Lit(ast::Lit::Null)) {
            self.cx.error(
                Diagnostic::error(
                    "`Object.create` with a prototype other than `null` is not supported yet",
                    proto.span,
                )
                .with_note("Velt objects get their prototype from their type: a class instance from its class, any other object from `Object.prototype`"),
            );
            self.check_args_loose(args);
            return self.error_expr(span);
        }
        let want = self.hint(exp);
        let record = want.filter(|t| {
            self.record_args(*t)
                .is_some_and(|(k, _)| k == self.cx.ty.str_)
        });
        let Some(record) = record else {
            let mut d = Diagnostic::error(
                "`Object.create(null)` makes a `Record<string, V>`: it needs a variable or parameter of that type",
                span,
            );
            if let Some(t) = want {
                let tn = self.cx.display(t);
                d = Diagnostic::error(
                    format!("`Object.create(null)` as a `{tn}` is not supported yet: it makes a `Record<string, V>`"),
                    span,
                );
            }
            self.cx.error(
                d.with_note("write `const d: Record<string, number> = Object.create(null);`"),
            );
            return self.error_expr(span);
        };
        let empty = synth(ast::ExprKind::Object(vec![]), span);
        let h = self.expr(&empty, Some(record), Want::Move);
        let mut lets = vec![];
        let r = self.temp(&format!("<create@{}>", span.lo), h, &mut lets);
        let mark = self.record_call(r.clone(), "__markNullProto", &[], span);
        lets.push(hir::Stmt {
            span,
            kind: hir::StmtKind::Expr(mark),
        });
        self.with_lets(lets, r)
    }
}

/// The `Symbol.toStringTag` of instances of standard library class `d` (`name`) or of classes
/// extending it (`None`: not known).
fn std_string_tag(s: &FnCx<'_, '_>, d: DefId, name: &str) -> Option<&'static str> {
    let is = |n: &str| s.cx.prelude_adt(n) == Some(d);
    Some(match name {
        "Map" if is("Map") => "Map",
        "WeakMap" if is("WeakMap") => "WeakMap",
        "WeakSet" if is("WeakSet") => "WeakSet",
        "Date" if is("Date") => "Date",
        "Set" => "Set",
        "RegExp" => "RegExp",
        _ if s
            .cx
            .prelude_adt("Error")
            .is_some_and(|e| s.cx.class_extends(d, e) || d == e) =>
        {
            "Error"
        }
        _ => return None,
    })
}

/// `e` without parentheses.
fn strip_parens(e: &ast::Expr) -> &ast::Expr {
    match &e.kind {
        ast::ExprKind::Paren(inner) => strip_parens(inner),
        _ => e,
    }
}
