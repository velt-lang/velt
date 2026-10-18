//! `Object.assign(target, ...sources)` (object_builtins.rs): each source's own fields written to
//! the target in order, through its setters (synthesized assignments `target.k = source.k`, so
//! the usual checks apply), and the target itself as the result. A record target takes any keys
//! (a record source through `__extend`); an object or class target only the fields its type
//! declares, since Velt objects can't gain fields. A variable or field path target is written in
//! place and is the result itself; an optional source field is copied only while present.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::setters::synth;
use crate::body::places::is_path;
use crate::body::{FnCx, Want};
use crate::hir::{self, AdtKind, ExprKind as H, TyId, TyKind};

impl FnCx<'_, '_> {
    /// Is `callee` the built-in `Object.assign`?
    pub(super) fn object_assign_callee(&mut self, callee: &ast::Expr) -> bool {
        match &callee.kind {
            ast::ExprKind::Member {
                object,
                prop,
                optional: false,
            } if prop.name == "assign" => match &object.kind {
                ast::ExprKind::Ident(ns) => self.is_prelude_object(ns),
                _ => false,
            },
            _ => false,
        }
    }

    /// `Object.assign(...)` (`object_assign_callee`), its value used as `want` says.
    pub(super) fn object_assign_call(
        &mut self,
        type_args: &[ast::TypeExpr],
        args: &[ast::Expr],
        want: Want,
        span: Span,
    ) -> hir::Expr {
        if !type_args.is_empty() {
            self.cx
                .err("`Object.assign` takes no type arguments".to_string(), span);
        }
        if let Some(h) = self.reject_dynamic_args("Object.assign", args, span) {
            return h;
        }
        self.object_assign(args, want, span)
    }

    /// `Object.assign(target, ...sources)`; `want`: how a target written in place is used.
    pub(super) fn object_assign(
        &mut self,
        args: &[ast::Expr],
        want: Want,
        span: Span,
    ) -> hir::Expr {
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
        // A record whose values can't be `undefined` takes only defined values.
        let record_defined_only = self
            .record_args(tty)
            .is_some_and(|(_, v)| !self.cx.ty.nothing_of(v).is_some_and(|n| n.has_undefined()));
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
        // A variable or a field path is written in place: a temporary would be a copy of an
        // object stored inline (one of numbers and booleans only), and the writes would miss it.
        let in_place = (!record && is_place(target) && is_path(&t)).then(|| target.clone());
        let t_read = match in_place {
            Some(_) => t,
            None => self.named_temp(&tname, t, &mut lets),
        };
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
                let target_read = match &in_place {
                    Some(e) => e.clone(),
                    None => read(&tname),
                };
                let place = match record {
                    true => ast::ExprKind::Index {
                        object: Box::new(target_read),
                        index: Box::new(synth(
                            ast::ExprKind::Lit(ast::Lit::Str(key.clone())),
                            span,
                        )),
                        optional: false,
                    },
                    false => ast::ExprKind::Member {
                        object: Box::new(target_read),
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
                // An optional field that is absent is not an own property: not copied
                // (`if ("k" in source) target.k = source.k;`). A place that can't hold
                // `undefined` doesn't take a present `undefined` either.
                let present = synth(
                    ast::ExprKind::Binary {
                        op: ast::BinaryOp::In,
                        lhs: Box::new(synth(ast::ExprKind::Lit(ast::Lit::Str(key.clone())), span)),
                        rhs: Box::new(read(&sname)),
                    },
                    span,
                );
                let defined_only = match record {
                    true => record_defined_only,
                    false => !self.slot_takes_undefined(tty, &key),
                };
                let cond = match defined_only {
                    false => present,
                    true => synth(
                        ast::ExprKind::Binary {
                            op: ast::BinaryOp::And,
                            lhs: Box::new(present),
                            rhs: Box::new(synth(
                                ast::ExprKind::Binary {
                                    op: ast::BinaryOp::StrictNotEq,
                                    lhs: Box::new(field.clone()),
                                    rhs: Box::new(synth(
                                        ast::ExprKind::Lit(ast::Lit::Undefined),
                                        span,
                                    )),
                                },
                                span,
                            )),
                        },
                        span,
                    ),
                };
                let stmt = match optional {
                    false => ast::Stmt {
                        kind: ast::StmtKind::Expr(assign),
                        span,
                    },
                    true => ast::Stmt {
                        kind: ast::StmtKind::If {
                            cond,
                            then: ast::Block {
                                stmts: vec![ast::Stmt {
                                    kind: ast::StmtKind::Expr(assign),
                                    span,
                                }],
                                span,
                            },
                            els: None,
                        },
                        span,
                    },
                };
                self.stmt(&stmt, &mut lets);
            }
        }
        self.pop_scope();
        // The place itself, read after the writes, as the consumer wants it.
        let t_read = match &in_place {
            Some(e) => self.expr(e, None, want),
            None => t_read,
        };
        self.with_lets(lets, t_read)
    }

    /// A temporary holding `init`, which synthesized code reads by `name` (in the current
    /// scope).
    pub(super) fn named_temp(
        &mut self,
        name: &str,
        init: hir::Expr,
        lets: &mut Vec<hir::Stmt>,
    ) -> hir::Expr {
        let read = self.temp(name, init, lets);
        if let (H::Local(l, _), Some(scope)) = (&read.kind, self.f.scopes.last_mut()) {
            scope.names.insert(name.to_string(), *l);
        }
        read
    }

    /// `target.__extend(source)` of two records.
    pub(super) fn record_entries_into(
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
    pub(super) fn assign_target_fields(&mut self, t: TyId, span: Span) -> Option<Vec<String>> {
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

    /// Can field `key` of the object type `t` hold `undefined` (`false` for a setter's key)?
    fn slot_takes_undefined(&mut self, t: TyId, key: &str) -> bool {
        let TyKind::Adt(d, _) = self.cx.ty.kind(t) else {
            return false;
        };
        let Some(a) = self.cx.adt(*d) else {
            return false;
        };
        a.fields.iter().find(|f| f.name == key).is_some_and(|f| {
            f.optional
                || self
                    .cx
                    .ty
                    .nothing_of(f.ty)
                    .is_some_and(|n| n.has_undefined())
        })
    }

    /// The own keys an `Object.assign` source of type `t` gives, each with whether it is an
    /// optional field (copied only while present), or `None` (reported at `span`).
    pub(super) fn assign_source_keys(
        &mut self,
        t: TyId,
        span: Span,
    ) -> Option<Vec<(String, bool)>> {
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
            if !a.fields.iter().any(|f| f.name == k.name) {
                return None;
            }
            out.push((k.name, k.optional && !class));
        }
        Some(out)
    }
}

/// Is `e` a variable, `this` or a path of fields of one (read again without effects)?
fn is_place(e: &ast::Expr) -> bool {
    match &e.kind {
        ast::ExprKind::Ident(_) | ast::ExprKind::This => true,
        ast::ExprKind::Paren(inner) => is_place(inner),
        ast::ExprKind::Member {
            object,
            optional: false,
            ..
        } => is_place(object),
        _ => false,
    }
}
