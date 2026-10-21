//! `Object.defineProperty(o, key, { value, enumerable, writable, configurable })` (#922): a
//! write of the field the property stands for, and `o` as the result.
//!
//! - A symbol brand (`key` a symbol constant): the field `[key]?: T` that `crate::defined_props`
//!   added to `o`'s declared type. Unless `enumerable: true`, the symbol names a hidden field
//!   (`hir::FieldDef::hidden`), as Node keeps a non-enumerable property out of `console.log` and
//!   spreads; unless `writable: true`, nothing else may write it. Either is checked by name: a
//!   property with that symbol written directly anywhere (`{ [key]: v }`, `p[key] = v`) is an
//!   error here.
//! - A string key naming a field `o`'s type declares, with only a `value`: the property keeps its
//!   attributes in JavaScript, so this is an assignment.
//!
//! Accessors (`get` / `set`) are `crate::defined_accessors`'s, which rewrites the ones it
//! follows; the others, other attributes on a declared field, and keys a type doesn't declare
//! are errors saying they are not supported yet. On `globalThis`, the call assigns the
//! property's variable (`crate::global_this`).

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::object_assign::{is_place, reads_place};
use super::object_builtins::strip_parens;
use super::setters::synth;
use crate::body::{FnCx, Want};
use crate::hir;
use crate::symbols::Presence;

/// The member `defineProperty` writes: `o[KEY]` (a symbol) or `o.name`.
enum Key {
    Index(Box<ast::Expr>),
    Member(ast::Ident),
}

/// A property descriptor written as an object literal.
#[derive(Default)]
struct Descriptor<'a> {
    value: Option<&'a ast::Expr>,
    enumerable: Option<bool>,
    writable: Option<bool>,
    configurable: Option<bool>,
}

impl FnCx<'_, '_> {
    /// `Object.defineProperty(o, key, descriptor)`.
    pub(super) fn object_define_property(&mut self, args: &[ast::Expr], span: Span) -> hir::Expr {
        let [o, key, desc] = args else {
            return self.arity_error("Object.defineProperty", 3, args, span);
        };
        let Some(d) = self.descriptor(desc) else {
            self.check_args_loose(args);
            return self.error_expr(span);
        };
        if let Some(key) = global_key(o, key, self) {
            return self.global_define(&key, &d, desc, span);
        }
        let Some(value) = d.value else {
            self.not_yet(
                "`Object.defineProperty` without a `value` is not supported yet",
                desc.span,
                "an accessor (`get` / `set`) defined after construction waits for object-literal accessors (#941)",
            );
            self.check_args_loose(args);
            return self.error_expr(span);
        };
        let Some(ot) = self.trial_type(o) else {
            self.check_args_loose(args);
            return self.error_expr(span);
        };
        let tn = self.cx.display(ot);
        let mut redefine = None;
        let (key_place, name) = match self.symbol_key_of(key) {
            Some(k) => {
                if self.is_class_instance(ot) {
                    // A class's optional field is an own property from construction: Node's
                    // `defineProperty` redefines it, with the attributes it already has.
                    let shown = self.cx.shown_name(&k);
                    self.not_yet(
                        &format!("`Object.defineProperty` of `{shown}` on a `{tn}`, a class instance, is not supported yet"),
                        key.span,
                        "a field the class declares is an own property from construction, which `defineProperty` redefines with the attributes it has; assign it instead",
                    );
                    self.check_args_loose(args);
                    return self.error_expr(span);
                }
                if let Some(at) = self.cx.direct_write(&k) {
                    let shown = self.cx.shown_name(&k);
                    self.cx.error(
                        Diagnostic::error(
                            format!("`Object.defineProperty` with the key `{shown}`, which the program also writes directly, is not supported yet"),
                            key.span,
                        )
                        .with_label(at, "written directly here")
                        .with_note("a property `defineProperty` makes is not enumerable and not writable unless its descriptor says so, which Velt keeps per symbol"),
                    );
                    self.check_args_loose(args);
                    return self.error_expr(span);
                }
                let own = (
                    d.enumerable == Some(true),
                    d.writable == Some(true),
                    d.configurable == Some(true),
                );
                let Some((_, writable, configurable)) = self.cx.brand_attrs(&k, own) else {
                    let shown = self.cx.shown_name(&k);
                    self.not_yet(
                        &format!("`Object.defineProperty` of `{shown}` with different attributes at different calls is not supported yet"),
                        desc.span,
                        "Velt keeps a symbol brand's `enumerable`, `writable` and `configurable` per symbol: give every call with this key the same ones",
                    );
                    self.check_args_loose(args);
                    return self.error_expr(span);
                };
                // Defined again with another value, a property neither writable nor
                // configurable throws Node's `TypeError`.
                redefine = (!writable && !configurable).then(|| {
                    let shown = crate::property_order::symbol_key(&k).unwrap_or_default();
                    let shown = shown.trim_start_matches('[').trim_end_matches(']');
                    format!("Cannot redefine property: {shown}")
                });
                (Key::Index(Box::new(key.clone())), k)
            }
            None => {
                if let ast::ExprKind::Ident(k) = &strip_parens(key).kind {
                    // A symbol no declared type has as a member: its brand has no field.
                    self.not_yet(
                        &format!("`Object.defineProperty` of `[{}]` on a `{tn}`, which doesn't declare it, is not supported yet", k.name),
                        key.span,
                        "Velt objects have the fields their type declares: declare the variable with an interface or object type (`const o: T = { ... }`), to which a symbol brand's field is added",
                    );
                    self.check_args_loose(args);
                    return self.error_expr(span);
                }
                let ast::ExprKind::Lit(ast::Lit::Str(n)) = &strip_parens(key).kind else {
                    self.cx.err(
                        "`Object.defineProperty` with a key known only at run time is not supported yet: Velt objects have the fields their type declares",
                        key.span,
                    );
                    self.check_args_loose(args);
                    return self.error_expr(span);
                };
                if d.enumerable.is_some() || d.writable.is_some() || d.configurable.is_some() {
                    self.not_yet(
                        &format!("`Object.defineProperty` changing the attributes of `{n}` is not supported yet"),
                        desc.span,
                        "with only a `value`, it sets the field (the property keeps its attributes, as in JavaScript)",
                    );
                    self.check_args_loose(args);
                    return self.error_expr(span);
                }
                let prop = ast::Ident {
                    name: n.clone(),
                    span: key.span,
                };
                (Key::Member(prop), n.clone())
            }
        };
        match self.cx.declared_presence(ot, &name) {
            Some(Presence::Yes | Presence::Optional(_)) => {}
            _ => {
                let shown = self.cx.shown_name(&name);
                self.not_yet(
                    &format!("`Object.defineProperty` of `{shown}` on a `{tn}`, which doesn't declare it, is not supported yet"),
                    key.span,
                    "Velt objects have the fields their type declares: declare the variable with an interface or object type (`const o: T = { ... }`), to which a symbol brand's field is added, or declare the field",
                );
                self.check_args_loose(args);
                return self.error_expr(span);
            }
        }
        // `o[key] = value; o` for a variable or a path of fields and elements of one (each
        // index evaluated once), written in place: a temporary would be a copy of an object
        // stored inline, and the write would miss it. Otherwise
        // `(<define> = o, <define>[key] = value, <define>)`.
        self.push_scope();
        let mut lets = vec![];
        let mut value = value.clone();
        let obj = self.expr(o, None, Want::Borrow);
        let in_place =
            (is_place(o) && reads_place(&obj)).then(|| self.place_once(o, span, &mut lets));
        let (base, read) = match &in_place {
            Some(p) => (p.clone(), None),
            None => {
                let read = self.named_temp(&temp_ident(span).name, obj, &mut lets);
                (
                    synth(ast::ExprKind::Ident(temp_ident(span)), span),
                    Some(read),
                )
            }
        };
        let target = |base: ast::Expr| {
            let kind = match &key_place {
                Key::Index(k) => ast::ExprKind::Index {
                    object: Box::new(base),
                    index: k.clone(),
                    optional: false,
                },
                Key::Member(p) => ast::ExprKind::Member {
                    object: Box::new(base),
                    prop: p.clone(),
                    optional: false,
                },
            };
            synth(kind, key.span)
        };
        if let (Some(msg), Key::Index(k)) = (&redefine, &key_place) {
            self.redefine_check(
                msg,
                k,
                &base,
                target(base.clone()),
                &mut value,
                span,
                &mut lets,
            );
        }
        let assign = synth(
            ast::ExprKind::Assign {
                op: None,
                target: Box::new(target(base.clone())),
                value: Box::new(value),
            },
            span,
        );
        let h = self.expr_stmt(&assign);
        lets.push(hir::Stmt {
            span,
            kind: hir::StmtKind::Expr(h),
        });
        // The object itself, read after the write.
        let read = match read {
            Some(r) => r,
            None => self.expr(&base, None, Want::Borrow),
        };
        self.pop_scope();
        self.with_lets(lets, read)
    }

    /// `Object.defineProperty(globalThis, "K", d)`: an assignment of the variable standing for
    /// property `K` (`crate::global_this`). `enumerable` changes nothing Velt shows: the keys of
    /// `globalThis` are not listed.
    fn global_define(
        &mut self,
        key: &str,
        d: &Descriptor,
        desc: &ast::Expr,
        span: Span,
    ) -> hir::Expr {
        let Some(value) = d.value else {
            self.not_yet(
                "`Object.defineProperty` on `globalThis` without a `value` is not supported yet",
                desc.span,
                "give the property its value: `{ value: v, writable: true, configurable: true }`",
            );
            return self.error_expr(span);
        };
        let writable = d.writable == Some(true);
        if !writable && d.configurable != Some(true) {
            self.not_yet(
                &format!("`Object.defineProperty` of `globalThis.{key}` neither writable nor configurable is not supported yet"),
                desc.span,
                "Velt keeps the property as a variable, which it can't make read-only: add `writable: true` or `configurable: true`",
            );
            return self.error_expr(span);
        }
        if let (false, Some(at)) = (writable, self.cx.global_props.direct_write(key)) {
            self.cx.error(
                Diagnostic::error(
                    format!("`Object.defineProperty` of `globalThis.{key}` without `writable: true`, which the program also assigns, is not supported yet"),
                    desc.span,
                )
                .with_label(at, "assigned here")
                .with_note("in Node, assigning a property `defineProperty` made read-only throws a `TypeError`: add `writable: true`, or define it again instead of assigning it"),
            );
            return self.error_expr(span);
        }
        if !self.cx.global_props.slots.contains_key(key) {
            return self.error_expr(span); // reported by the rewrite: no type
        }
        let slot = ast::Ident {
            name: crate::global_this::slot_name(key),
            span,
        };
        let assign = synth(
            ast::ExprKind::Assign {
                op: None,
                target: Box::new(synth(ast::ExprKind::Ident(slot), span)),
                value: Box::new(value.clone()),
            },
            span,
        );
        let h = self.expr_stmt(&assign);
        let stmt = hir::Stmt {
            span,
            kind: hir::StmtKind::Expr(h),
        };
        let unit = self.mk(hir::ExprKind::Lit(hir::Lit::Unit), self.cx.ty.unit, span);
        self.with_lets(vec![stmt], unit)
    }

    /// `if (key in o && o[key] !== value) throw new TypeError(msg)` (appended to `lets`): a
    /// property neither writable nor configurable defined again with another value. `value` is
    /// read twice: a value other than a literal is first held in a temporary.
    #[allow(clippy::too_many_arguments)]
    fn redefine_check(
        &mut self,
        msg: &str,
        key: &ast::Expr,
        base: &ast::Expr,
        current: ast::Expr,
        value: &mut ast::Expr,
        span: Span,
        lets: &mut Vec<hir::Stmt>,
    ) {
        if !matches!(strip_parens(value).kind, ast::ExprKind::Lit(_)) {
            let v = self.expr(value, None, Want::Move);
            let name = format!("<define@{}:value>", span.lo);
            self.named_temp(&name, v, lets);
            *value = synth(ast::ExprKind::Ident(ast::Ident { name, span }), value.span);
        }
        let bin = |op, lhs, rhs| {
            synth(
                ast::ExprKind::Binary {
                    op,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                },
                span,
            )
        };
        let cond = bin(
            ast::BinaryOp::And,
            bin(ast::BinaryOp::In, key.clone(), base.clone()),
            // SameValue (`Object.is`): `NaN` is itself, `-0` is not `0`.
            synth(
                ast::ExprKind::Unary {
                    op: ast::UnaryOp::Not,
                    expr: Box::new(synth(
                        ast::ExprKind::Call {
                            callee: Box::new(synth(
                                ast::ExprKind::Member {
                                    object: Box::new(synth(
                                        ast::ExprKind::Ident(ast::Ident {
                                            name: "Object".to_string(),
                                            span,
                                        }),
                                        span,
                                    )),
                                    prop: ast::Ident {
                                        name: "is".to_string(),
                                        span,
                                    },
                                    optional: false,
                                },
                                span,
                            )),
                            type_args: vec![],
                            args: vec![current, value.clone()],
                            optional: false,
                        },
                        span,
                    )),
                },
                span,
            ),
        );
        let error = synth(
            ast::ExprKind::New {
                class: ast::TypeExpr {
                    kind: ast::TypeExprKind::Named {
                        path: vec![ast::Ident {
                            name: "TypeError".to_string(),
                            span,
                        }],
                        args: vec![],
                    },
                    span,
                },
                args: vec![synth(
                    ast::ExprKind::Lit(ast::Lit::Str(msg.to_string())),
                    span,
                )],
            },
            span,
        );
        let stmt = ast::Stmt {
            kind: ast::StmtKind::If {
                cond,
                then: ast::Block {
                    stmts: vec![ast::Stmt {
                        kind: ast::StmtKind::Throw(error),
                        span,
                    }],
                    span,
                },
                els: None,
            },
            span,
        };
        self.stmt(&stmt, lets);
    }

    /// The attributes a descriptor literal gives (`None`, reported: not one Velt reads).
    fn descriptor<'a>(&mut self, desc: &'a ast::Expr) -> Option<Descriptor<'a>> {
        let ast::ExprKind::Object(props) = &strip_parens(desc).kind else {
            self.cx.err(
                "`Object.defineProperty` with a descriptor that is not an object literal is not supported yet",
                desc.span,
            );
            return None;
        };
        let mut d = Descriptor::default();
        for p in props {
            let (name, v) = match p {
                ast::ObjectProp::KeyValue(n, v) => (n, Some(v)),
                ast::ObjectProp::Shorthand(n) => (n, None),
                ast::ObjectProp::Method(m) => (&m.sig.name, None),
                ast::ObjectProp::Spread(e) => {
                    self.cx.err(
                        "a spread in a property descriptor is not supported yet",
                        e.span,
                    );
                    return None;
                }
            };
            match (name.name.as_str(), v) {
                ("value", Some(v)) => d.value = Some(v),
                (flag @ ("enumerable" | "writable" | "configurable"), Some(v)) => {
                    let ast::ExprKind::Lit(ast::Lit::Bool(b)) = strip_parens(v).kind else {
                        self.cx.err(
                            format!("`{flag}` in a property descriptor must be `true` or `false` while compiling"),
                            v.span,
                        );
                        return None;
                    };
                    match flag {
                        "enumerable" => d.enumerable = Some(b),
                        "writable" => d.writable = Some(b),
                        _ => d.configurable = Some(b),
                    }
                }
                ("get" | "set", _) => {
                    self.not_yet(
                        "an accessor defined with `Object.defineProperty` is not supported yet here",
                        name.span,
                        "Velt supports it as a statement on a `const o: I = { get k() { … } }` of the same function, `I` an interface declaring the accessor: the call replaces the literal's accessors",
                    );
                    return None;
                }
                (other, _) => {
                    self.cx.err(
                        format!("`{other}` is not a property descriptor attribute (`value`, `enumerable`, `writable`, `configurable`, `get`, `set`)"),
                        name.span,
                    );
                    return None;
                }
            }
        }
        Some(d)
    }

    /// Is `t` an instance of a class?
    fn is_class_instance(&mut self, t: hir::TyId) -> bool {
        match self.cx.ty.kind(t) {
            hir::TyKind::Adt(d, _) => {
                let d = *d;
                self.cx
                    .adt(d)
                    .is_some_and(|a| a.kind == hir::AdtKind::Class)
            }
            _ => false,
        }
    }

    /// Report `msg` at `span` with `note`.
    fn not_yet(&mut self, msg: &str, span: Span, note: &str) {
        self.cx
            .error(Diagnostic::error(msg.to_string(), span).with_note(note.to_string()));
    }
}

/// The key of `Object.defineProperty(globalThis, "K", d)`, as `crate::global_this` normalizes
/// it (`None`: another object).
fn global_key(o: &ast::Expr, key: &ast::Expr, cx: &FnCx) -> Option<String> {
    let ast::ExprKind::Ident(id) = &o.kind else {
        return None;
    };
    let ast::ExprKind::Lit(ast::Lit::Str(k)) = &key.kind else {
        return None;
    };
    (id.name == "globalThis" && !cx.is_local_name(&id.name)).then(|| k.clone())
}

/// The temporary holding the object of the `defineProperty` call at `span`.
fn temp_ident(span: Span) -> ast::Ident {
    ast::Ident {
        name: format!("<define@{}>", span.lo),
        span,
    }
}
