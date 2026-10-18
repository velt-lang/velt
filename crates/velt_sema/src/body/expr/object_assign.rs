//! `Object.assign(target, ...sources)` (object_builtins.rs): each source's own fields written to
//! the target in order, through its setters (synthesized assignments `target.k = source.k`, so
//! the usual checks apply), and the target itself as the result. A record target takes any keys
//! (a record source through `__extend`); an object or class target only the fields its type
//! declares, since Velt objects can't gain fields.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::setters::synth;
use crate::body::{FnCx, Want};
use crate::hir::{self, AdtKind, ExprKind as H, TyId, TyKind};

impl FnCx<'_, '_> {
    /// `Object.assign(target, ...sources)`.
    pub(super) fn object_assign(&mut self, args: &[ast::Expr], span: Span) -> hir::Expr {
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
}
