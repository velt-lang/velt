//! Defaults in destructuring declarations (`const { a = 1, b } = opts;`, `const [x = 0] = xs;`).
//! A declaration whose pattern has a default anywhere is rewritten into plain declarations
//! through hidden temporaries, each checked before the next is built, so field and element types
//! are known. As in JavaScript, a default applies where the value is `undefined`: a field's
//! when it is `undefined` or, for an optional field, absent (`opts.a ?? 1`; a declared
//! `T | null` field keeps its `null`, also an optional one, `a?: T | null`, which is tested
//! with its presence flag: `"a" in opts ? opts.a : 1`), an array element's when the array is too short
//! (`#t.length > 0 ? #t[0] : 0`) or the element is `undefined`
//! (`(#t.length > 0 ? #t[0] : undefined) ?? 0`), and a tuple element's when it is `undefined`.
//! A non-array iterable is first collected into a second temporary holding the values the
//! pattern needs (`consume.rs`).

use velt_common::Span;
use velt_syntax::ast::{self, Expr, ExprKind as E, Pattern, PatternKind as P, VarDecl, VarKind};

use super::FnCx;
use crate::hir;

/// Does `p` contain a default?
pub(super) fn has_default(p: &Pattern) -> bool {
    match &p.kind {
        P::Default { .. } => true,
        P::Object { fields, .. } => fields.iter().any(|(_, f)| has_default(f)),
        P::Array { elems, .. } => elems.iter().any(has_default),
        P::Ident(_) | P::Wildcard => false,
    }
}

impl FnCx<'_, '_> {
    /// `kind pattern = init;` for a pattern with defaults (see the module docs).
    pub(super) fn decl_with_defaults(
        &mut self,
        kind: VarKind,
        pattern: &Pattern,
        init: Expr,
        out: &mut Vec<hir::Stmt>,
    ) {
        match &pattern.kind {
            P::Object { fields, rest } => {
                if let Some(r) = rest {
                    self.cx
                        .err("`...rest` in object patterns is not supported yet", r.span);
                }
                let tmp = self.temp_decl(init, out);
                for (key, sub) in fields {
                    let field = member(&tmp, key.clone());
                    let (sub, value) = match self.split_default(sub, &tmp, key) {
                        Some((inner, Applies::WhenNothing(d))) => (inner, nullish(field, d)),
                        Some((inner, Applies::WhenAbsent(d))) => {
                            (inner, cond_expr(has_key(key, &tmp), field, d))
                        }
                        Some((inner, Applies::Never)) => (inner, field),
                        None => (sub, field),
                    };
                    self.decl_with_defaults(kind, sub, value, out);
                }
            }
            P::Array { elems, rest } => {
                let tmp = self.temp_decl(init, out);
                if self.is_tuple_value(&tmp) {
                    // A tuple always has its elements: the defaults never apply.
                    return self.tuple_decl(kind, pattern, tmp, out);
                }
                let tmp = self.collected_temp(tmp, rest.is_none().then_some(elems.len()), out);
                let undefined_elems = self.temp_ty(&tmp).is_some_and(|t| {
                    let elem = match self.cx.ty.kind(t) {
                        hir::TyKind::Array(e) => *e,
                        _ => return false,
                    };
                    self.has_undefined(elem)
                });
                for (k, sub) in elems.iter().enumerate() {
                    let elem = index(&tmp, k);
                    let value = match &sub.kind {
                        // An element that may be `undefined` takes the default then too.
                        P::Default { pattern, value } if undefined_elems => {
                            let cond = longer_than(&tmp, k);
                            let undefined = mk(E::Lit(ast::Lit::Undefined), elem.span);
                            let read = cond_expr(cond, elem, undefined);
                            (pattern.as_ref(), nullish(read, (**value).clone()))
                        }
                        P::Default { pattern, value } => {
                            let cond = longer_than(&tmp, k);
                            (pattern.as_ref(), cond_expr(cond, elem, (**value).clone()))
                        }
                        _ => (sub, elem),
                    };
                    self.decl_with_defaults(kind, value.0, value.1, out);
                }
                if let Some(r) = rest {
                    let slice = call(
                        member(&tmp, ident("slice", r.span)),
                        vec![int(elems.len(), r.span)],
                    );
                    let p = pat(P::Ident(r.clone()), r.span);
                    self.decl_with_defaults(kind, &p, slice, out);
                }
            }
            P::Default { pattern, value } => {
                let v = nullish(init, (**value).clone());
                self.decl_with_defaults(kind, pattern, v, out);
            }
            P::Ident(_) | P::Wildcard => {
                let span = pattern.span;
                let v = VarDecl {
                    kind,
                    pattern: pattern.clone(),
                    ty: None,
                    init: Some(init),
                    span,
                };
                self.var_decl(&v, span, out);
            }
        }
    }

    /// `const #dN = init;`: the hidden temporary a pattern is taken apart from.
    fn temp_decl(&mut self, init: Expr, out: &mut Vec<hir::Stmt>) -> Expr {
        self.temp_with(init, None, out)
    }

    /// `const #dN: T = init;` for an annotated declaration with defaults.
    pub(super) fn typed_temp(
        &mut self,
        init: &Expr,
        ty: &ast::TypeExpr,
        out: &mut Vec<hir::Stmt>,
    ) -> Expr {
        self.temp_with(init.clone(), Some(ty.clone()), out)
    }

    fn temp_with(
        &mut self,
        init: Expr,
        ty: Option<ast::TypeExpr>,
        out: &mut Vec<hir::Stmt>,
    ) -> Expr {
        let span = init.span;
        let n = self.f.locals.len();
        let name = ident(&format!("#d{n}"), span);
        let v = VarDecl {
            kind: VarKind::Const,
            pattern: pat(P::Ident(name.clone()), span),
            ty,
            init: Some(init),
            span,
        };
        self.var_decl(&v, span, out);
        mk(E::Ident(name), span)
    }

    /// `{ a = d }` on field `key` of temporary `tmp`: the inner pattern, and when the default
    /// applies. On a field that is never `undefined` or absent it never does (TS accepts it
    /// too).
    fn split_default<'p>(
        &mut self,
        sub: &'p Pattern,
        tmp: &Expr,
        key: &ast::Ident,
    ) -> Option<(&'p Pattern, Applies)> {
        let P::Default { pattern, value } = &sub.kind else {
            return None;
        };
        let ty = self.temp_ty(tmp)?;
        // An optional field's `null` is its absence (JavaScript's `undefined`) in this phase,
        // unless it is declared `a?: T | null`: that one keeps a presence flag, and its `null`
        // is a value the default must not replace.
        let optional = |cx: &crate::ctx::Ctx, i: u32| match cx.ty.kind(ty) {
            hir::TyKind::Adt(d, _) => cx.adt(*d).and_then(|a| {
                let f = a.fields.get(i as usize).filter(|f| f.optional)?;
                let flag = crate::anon::has_presence(&cx.ty, a.kind, f);
                Some((flag, cx.ty.opt_payload(f.declared).is_some()))
            }),
            _ => None,
        };
        let default = || (**value).clone();
        let applies = match self.field_of(ty, &key.name) {
            Some((i, t)) => match (self.cx.ty.nothing_of(t), optional(self.cx, i)) {
                (Some(n), _) if n.has_undefined() => Applies::WhenNothing(default()),
                (Some(_), Some((true, _))) => Applies::WhenAbsent(default()),
                // A class keeps no presence flag: its `a?: T | null` cannot tell an unset
                // field from one set to `null`.
                (Some(_), Some((false, true))) => {
                    self.cx.error(
                        velt_common::Diagnostic::error(
                            format!(
                                "a default for `{}`, an optional class field declared `T | null`, is not supported yet (#930)",
                                key.name
                            ),
                            value.span,
                        )
                        .with_note(
                            "the default applies when the field is unset but not when it is `null`, and a class field does not keep the two apart yet",
                        )
                        .with_note(format!(
                            "read the field and apply the default yourself: `{}.{} ?? …` replaces both",
                            "obj", key.name
                        )),
                    );
                    Applies::Never
                }
                (Some(_), Some((false, false))) => Applies::WhenNothing(default()),
                _ => Applies::Never,
            },
            None => Applies::Never,
        };
        Some((pattern, applies))
    }

    /// Can a value of type `t` be `undefined`?
    fn has_undefined(&self, t: hir::TyId) -> bool {
        self.cx.ty.nothing_of(t).is_some_and(|n| n.has_undefined())
    }

    /// `kind [a, b = d] = tmp;` on a tuple: an ordinary destructuring without the defaults,
    /// except for elements that may be `undefined` (`b = tmp[1] ?? d`).
    fn tuple_decl(&mut self, kind: VarKind, p: &Pattern, tmp: Expr, out: &mut Vec<hir::Stmt>) {
        let span = p.span;
        let elem_tys = match self.temp_ty(&tmp).map(|t| self.cx.ty.kind(t).clone()) {
            Some(hir::TyKind::Tuple(ts)) => ts,
            _ => vec![],
        };
        if let P::Array { elems, rest } = &p.kind {
            let applies = elems.iter().enumerate().any(|(k, e)| {
                matches!(e.kind, P::Default { .. })
                    && elem_tys.get(k).is_some_and(|t| self.has_undefined(*t))
            });
            if applies {
                if let Some(r) = rest {
                    self.cx.err(
                        "`...rest` next to a default of an element that may be `undefined` is not supported yet",
                        r.span,
                    );
                }
                for (k, sub) in elems.iter().enumerate().take(elem_tys.len()) {
                    let elem = index(&tmp, k);
                    let (sub, value) = match &sub.kind {
                        P::Default { pattern, value } if self.has_undefined(elem_tys[k]) => {
                            (pattern.as_ref(), nullish(elem, (**value).clone()))
                        }
                        P::Default { pattern, .. } => (pattern.as_ref(), elem),
                        _ => (sub, elem),
                    };
                    self.decl_with_defaults(kind, sub, value, out);
                }
                return;
            }
        }
        let v = VarDecl {
            kind,
            pattern: without_defaults(p),
            ty: None,
            init: Some(tmp),
            span,
        };
        self.var_decl(&v, span, out);
    }

    /// A temporary holding a non-array iterable (`const [a, b = 0] = gen()`): a second one
    /// holding the values the pattern needs (at most `limit`), as an array (`consume.rs`).
    fn collected_temp(
        &mut self,
        tmp: Expr,
        limit: Option<usize>,
        out: &mut Vec<hir::Stmt>,
    ) -> Expr {
        let E::Ident(name) = &tmp.kind else {
            return tmp;
        };
        let Some(local) = self.lookup_local(&name.name, name.span) else {
            return tmp;
        };
        let ty = self.f.locals[local.0 as usize].ty;
        if !self.is_consumable(ty) {
            return tmp;
        }
        let span = tmp.span;
        let src = self.mk(hir::ExprKind::Local(local, hir::UseMode::Borrow), ty, span);
        let values = match self.consumable(src) {
            Some(c) => self.collect(c, limit),
            None => self.error_expr(span),
        };
        let name = ident(&format!("#d{}", self.f.locals.len()), span);
        self.hidden_local(name.clone(), values, false, out);
        mk(E::Ident(name), span)
    }

    fn is_tuple_value(&mut self, tmp: &Expr) -> bool {
        self.temp_ty(tmp)
            .is_some_and(|t| matches!(self.cx.ty.kind(t), hir::TyKind::Tuple(_)))
    }

    /// The type of a temporary declared by `temp_decl`.
    fn temp_ty(&mut self, tmp: &Expr) -> Option<hir::TyId> {
        let E::Ident(name) = &tmp.kind else {
            return None;
        };
        let local = self.lookup_local(&name.name, name.span)?;
        Some(self.f.locals[local.0 as usize].ty)
    }
}

/// When an object pattern's default (the expression inside) applies to its field.
enum Applies {
    /// Never: the field always has a value, or is a declared `T | null`.
    Never,
    /// When the field is `undefined`, or the `null` that stands for an absent optional field:
    /// `field ?? default`.
    WhenNothing(Expr),
    /// When the optional field (`a?: T | null`) is absent: `"a" in tmp ? field : default`.
    WhenAbsent(Expr),
}

/// `p` with every default removed (`[a, b = 1]` → `[a, b]`).
fn without_defaults(p: &Pattern) -> Pattern {
    let kind = match &p.kind {
        P::Default { pattern, .. } => return without_defaults(pattern),
        P::Object { fields, rest } => P::Object {
            fields: fields
                .iter()
                .map(|(k, f)| (k.clone(), without_defaults(f)))
                .collect(),
            rest: rest.clone(),
        },
        P::Array { elems, rest } => P::Array {
            elems: elems.iter().map(without_defaults).collect(),
            rest: rest.clone(),
        },
        k => k.clone(),
    };
    pat(kind, p.span)
}

fn mk(kind: E, span: Span) -> Expr {
    Expr {
        id: ast::NodeId(u32::MAX),
        kind,
        span,
    }
}

fn pat(kind: P, span: Span) -> Pattern {
    Pattern {
        id: ast::NodeId(u32::MAX),
        kind,
        span,
    }
}

fn ident(name: &str, span: Span) -> ast::Ident {
    ast::Ident {
        name: name.to_string(),
        span,
    }
}

fn member(object: &Expr, prop: ast::Ident) -> Expr {
    let span = object.span;
    mk(
        E::Member {
            object: Box::new(object.clone()),
            prop,
            optional: false,
        },
        span,
    )
}

fn int(n: usize, span: Span) -> Expr {
    mk(
        E::Lit(ast::Lit::Int {
            value: n as u128,
            suffix: None,
        }),
        span,
    )
}

fn index(object: &Expr, k: usize) -> Expr {
    let span = object.span;
    mk(
        E::Index {
            object: Box::new(object.clone()),
            index: Box::new(int(k, span)),
            optional: false,
        },
        span,
    )
}

fn call(callee: Expr, args: Vec<Expr>) -> Expr {
    let span = callee.span;
    mk(
        E::Call {
            callee: Box::new(callee),
            type_args: vec![],
            args,
            optional: false,
        },
        span,
    )
}

fn nullish(lhs: Expr, rhs: Expr) -> Expr {
    let span = lhs.span;
    mk(
        E::Binary {
            op: ast::BinaryOp::Nullish,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        },
        span,
    )
}

/// `"key" in tmp`
fn has_key(key: &ast::Ident, tmp: &Expr) -> Expr {
    let span = tmp.span;
    mk(
        E::Binary {
            op: ast::BinaryOp::In,
            lhs: Box::new(mk(E::Lit(ast::Lit::Str(key.name.clone())), key.span)),
            rhs: Box::new(tmp.clone()),
        },
        span,
    )
}

/// `tmp.length > k`
fn longer_than(tmp: &Expr, k: usize) -> Expr {
    let span = tmp.span;
    mk(
        E::Binary {
            op: ast::BinaryOp::Gt,
            lhs: Box::new(member(tmp, ident("length", span))),
            rhs: Box::new(int(k, span)),
        },
        span,
    )
}

fn cond_expr(cond: Expr, then: Expr, els: Expr) -> Expr {
    let span = then.span;
    mk(
        E::Cond {
            cond: Box::new(cond),
            then: Box::new(then),
            els: Box::new(els),
        },
        span,
    )
}
