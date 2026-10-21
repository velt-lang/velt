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
                let tmp = self.temp_decl(init, out);
                if self
                    .temp_ty(&tmp)
                    .is_some_and(|t| self.cx.dynamic(t).is_some())
                {
                    return self.any_pattern_decls(kind, pattern, &tmp, out);
                }
                // `const { 0: a, length } = xs` of an array or a tuple (`index_keys.rs`).
                if self.index_key_decls(kind, fields, rest.as_ref(), &tmp, out) {
                    return;
                }
                self.object_decls(kind, fields, rest.as_ref(), &tmp, out);
            }
            P::Array { elems, rest } => {
                let tmp = self.temp_decl(init, out);
                if self
                    .temp_ty(&tmp)
                    .is_some_and(|t| self.cx.dynamic(t).is_some())
                {
                    return self.any_pattern_decls(kind, pattern, &tmp, out);
                }
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
                let dynamic_elems = self.temp_ty(&tmp).is_some_and(|t| {
                    matches!(self.cx.ty.kind(t), hir::TyKind::Array(e) if self.cx.dynamic(*e).is_some())
                });
                for (k, sub) in elems.iter().enumerate() {
                    // A hole reads nothing (`[, , c = 0] = xs` of a shorter array).
                    if matches!(sub.kind, P::Wildcard) {
                        continue;
                    }
                    let elem = index(&tmp, k);
                    let value = match &sub.kind {
                        // An `any` element holding `undefined` takes the default too.
                        P::Default { pattern, value } if dynamic_elems => {
                            let cond = longer_than(&tmp, k);
                            let undefined = mk(E::Lit(ast::Lit::Undefined), elem.span);
                            let read = cond_expr(cond, elem.clone(), undefined);
                            let d = (**value).clone();
                            let d = if matches!(d.kind, E::Lit(ast::Lit::Undefined)) {
                                read
                            } else {
                                cond_expr(is_undefined(&read), as_any(d), read)
                            };
                            (pattern.as_ref(), d)
                        }
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

    /// `kind { k1: p1, k2 = d, ...rest } = tmp;` key by key, each binding declared from
    /// `tmp.k` (with its default), then the rest. `tmp` is a variable or a path of fields: the
    /// hidden temporary of a pattern with defaults or a rest, or the value a method pattern
    /// takes apart (`property_pattern.rs`).
    pub(super) fn object_decls(
        &mut self,
        kind: VarKind,
        fields: &[(ast::Ident, Pattern)],
        rest: Option<&ast::Ident>,
        tmp: &Expr,
        out: &mut Vec<hir::Stmt>,
    ) {
        for (key, sub) in fields {
            let field = member(tmp, key.clone());
            let (sub, value) = match self.split_default(sub, tmp, key) {
                Some((inner, Applies::WhenNothing(d))) => (inner, nullish(field, d)),
                Some((inner, Applies::WhenAbsent(d))) => {
                    (inner, cond_expr(has_key(key, tmp), field, d))
                }
                Some((inner, Applies::WhenUndefined(d))) => (
                    inner,
                    cond_expr(is_undefined(&field), as_any(d), field.clone()),
                ),
                Some((inner, Applies::Never)) => (inner, field),
                None => (sub, field),
            };
            self.decl_with_defaults(kind, sub, value, out);
        }
        if let Some(r) = rest {
            let taken: Vec<String> = fields.iter().map(|(k, _)| k.name.clone()).collect();
            self.object_rest_decl(kind, r, &taken, tmp, out);
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
            // An `any` field holding `undefined` (also an absent `a?: any`).
            Some((_, t)) if self.cx.dynamic(t).is_some() => Applies::WhenUndefined(default()),
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
            // A `Record` (or index signature) key reads `V | undefined`.
            None if self.record_args(ty).is_some() => Applies::WhenNothing(default()),
            None => Applies::Never,
        };
        Some((pattern, applies))
    }

    /// Can a value of type `t` be `undefined`?
    pub(super) fn has_undefined(&self, t: hir::TyId) -> bool {
        self.cx.ty.nothing_of(t).is_some_and(|n| n.has_undefined())
    }

    /// `kind [a, b = d] = tmp;` on a tuple: an ordinary destructuring without the defaults,
    /// except for elements that may be `undefined` (`b = tmp[1] ?? d`). A tuple always has its
    /// elements, so a default directly on one applies only then; when one does, or an element's
    /// own pattern has defaults, numeric keys or an object rest (`[i, { a = 5 }]`,
    /// `[i, { 0: c }]`), each element is taken apart from `tmp[k]` on its own.
    fn tuple_decl(&mut self, kind: VarKind, p: &Pattern, tmp: Expr, out: &mut Vec<hir::Stmt>) {
        let span = p.span;
        let tuple_ty = self.temp_ty(&tmp);
        let elem_tys = match tuple_ty.map(|t| self.cx.ty.kind(t).clone()) {
            Some(hir::TyKind::Tuple(ts)) => ts,
            _ => vec![],
        };
        if let P::Array { elems, rest } = &p.kind {
            let applies = elems.iter().enumerate().any(|(k, e)| {
                matches!(e.kind, P::Default { .. })
                    && elem_tys.get(k).is_some_and(|t| self.has_undefined(*t))
            });
            let nested = elems.iter().any(|e| {
                let inner = match &e.kind {
                    P::Default { pattern, .. } => pattern,
                    _ => e,
                };
                has_default(inner)
                    || super::object_rest::has_object_rest(inner)
                    || super::index_keys::has_index_key(inner)
            });
            if applies || nested {
                if rest.is_some() || elems.len() > elem_tys.len() {
                    // What an ordinary pattern reports, without taking it apart again.
                    let shown = tuple_ty.map(|t| self.cx.display(t)).unwrap_or_default();
                    self.cx.err(
                        format!("pattern does not match the tuple type `{shown}`"),
                        span,
                    );
                    let reported = mk(E::Ident(ident(crate::body::REPORTED, span)), span);
                    let names = rest.iter().map(|r| pat(P::Ident(r.clone()), r.span));
                    for sub in elems.iter().cloned().chain(names) {
                        self.decl_with_defaults(
                            kind,
                            &without_defaults(&sub),
                            reported.clone(),
                            out,
                        );
                    }
                    return;
                }
                for (k, sub) in elems.iter().enumerate() {
                    if matches!(sub.kind, P::Wildcard) {
                        continue;
                    }
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
    pub(super) fn temp_ty(&mut self, tmp: &Expr) -> Option<hir::TyId> {
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
    /// When the `any` field holds `undefined`: `field === undefined ? default : field`.
    WhenUndefined(Expr),
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

pub(super) fn mk(kind: E, span: Span) -> Expr {
    Expr {
        id: ast::NodeId(u32::MAX),
        kind,
        span,
    }
}

pub(super) fn pat(kind: P, span: Span) -> Pattern {
    Pattern {
        id: ast::NodeId(u32::MAX),
        kind,
        span,
    }
}

pub(super) fn ident(name: &str, span: Span) -> ast::Ident {
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

/// The callee of `#undefined_or(value, default)`: `value`, or `default` where it is
/// `undefined` (not `null`, which a default never replaces). The name can't be written in
/// source; sema checks the call as `FnCx::undefined_or`.
pub(crate) const UNDEFINED_OR: &str = "#undefined_or";

/// A destructuring default: `#undefined_or(lhs, rhs)`; `lhs` itself for a default of
/// `undefined` (a destructuring assignment binds its places so, `assign_pattern.rs`).
fn nullish(lhs: Expr, rhs: Expr) -> Expr {
    if matches!(rhs.kind, E::Lit(ast::Lit::Undefined)) {
        return lhs;
    }
    let span = lhs.span;
    call(
        mk(E::Ident(ident(UNDEFINED_OR, span)), span),
        vec![lhs, rhs],
    )
}

/// `e as any`: a default for an `any` place is an `any` too, as in TypeScript.
fn as_any(e: Expr) -> Expr {
    let span = e.span;
    let ty = ast::TypeExpr {
        kind: ast::TypeExprKind::Named {
            path: vec![ident("any", span)],
            args: vec![],
        },
        span,
    };
    mk(
        E::Cast {
            expr: Box::new(e),
            ty,
        },
        span,
    )
}

/// `e === undefined`
fn is_undefined(e: &Expr) -> Expr {
    let span = e.span;
    mk(
        E::Binary {
            op: ast::BinaryOp::StrictEq,
            lhs: Box::new(e.clone()),
            rhs: Box::new(mk(E::Lit(ast::Lit::Undefined), span)),
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
