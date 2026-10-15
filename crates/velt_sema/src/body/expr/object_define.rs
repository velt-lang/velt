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
//! Accessors (`get` / `set`, #941), other attributes on a declared field, and keys a type doesn't
//! declare are errors saying they are not supported yet.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::object_builtins::strip_parens;
use super::setters::synth;
use crate::body::{FnCx, Want};
use crate::hir;
use crate::symbols::Presence;

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
        let (target, name) = match self.symbol_key_of(key) {
            Some(k) => {
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
                let place = ast::ExprKind::Index {
                    object: Box::new(synth(ast::ExprKind::Ident(temp_ident(span)), span)),
                    index: Box::new(key.clone()),
                    optional: false,
                };
                (place, k)
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
                let place = ast::ExprKind::Member {
                    object: Box::new(synth(ast::ExprKind::Ident(temp_ident(span)), span)),
                    prop: ast::Ident {
                        name: n.clone(),
                        span: key.span,
                    },
                    optional: false,
                };
                (place, n.clone())
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
        // `(<define> = o, <define>[key] = value, <define>)`.
        self.push_scope();
        let mut lets = vec![];
        let obj = self.expr(o, None, Want::Borrow);
        let read = self.named_temp(&temp_ident(span).name, obj, &mut lets);
        let assign = synth(
            ast::ExprKind::Assign {
                op: None,
                target: Box::new(synth(target, key.span)),
                value: Box::new(value.clone()),
            },
            span,
        );
        let h = self.expr_stmt(&assign);
        lets.push(hir::Stmt {
            span,
            kind: hir::StmtKind::Expr(h),
        });
        self.pop_scope();
        self.with_lets(lets, read)
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
                        "an accessor defined with `Object.defineProperty` is not supported yet",
                        name.span,
                        "it waits for getters and setters in object literals (#941)",
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

    /// Report `msg` at `span` with `note`.
    fn not_yet(&mut self, msg: &str, span: Span, note: &str) {
        self.cx
            .error(Diagnostic::error(msg.to_string(), span).with_note(note.to_string()));
    }
}

/// The temporary holding the object of the `defineProperty` call at `span`.
fn temp_ident(span: Span) -> ast::Ident {
    ast::Ident {
        name: format!("<define@{}>", span.lo),
        span,
    }
}
