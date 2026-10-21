//! An array pattern taking apart an array literal (`const [a, s] = [1, "s"]`, and the hidden
//! declaration of a destructuring assignment, `[x, s] = [x + 1, s + "!"]`): as in TypeScript,
//! the pattern types the literal as a tuple, so each element keeps its own type instead of the
//! union of all of them. The elements are evaluated in order into hidden temporaries, then each
//! is bound to its pattern (with its default), as JavaScript evaluates the whole literal before
//! taking it apart:
//!
//! ```text
//! const [a, b = d] = [e1, e2];   →   const #d1 = e1; const #d2 = e2; const a = #d1; const b = #d2 ?? d;
//! ```
//!
//! Only a literal without spreads or holes, at least as long as the pattern, and a pattern
//! without a rest; `[…] as const` counts as the literal. A literal of literals of one type
//! (`[10, 20]`) already has their type and is taken apart as an array.
//!
//! A pattern taking apart an `any` (`const [a, b] = v`, `const { p, q = 1 } = v`) reads each
//! element or property through the `any` (`v[0]`, `v.p`), each an `any`, as TypeScript types
//! them; a default applies where the read gives `undefined` (`v.q === undefined ? 1 : v.q`).
//!
//! An object pattern with a computed key (`const { [k]: v } = r`, the parser's `[k]` key) reads
//! each property the same way: `r[k]` for a key naming a variable, the member a `[KEY]` naming
//! a module constant (a string or a symbol) stands for.

use velt_syntax::ast::{self, Expr, ExprKind as E, PatternKind as P, VarDecl, VarKind};

use super::pattern_defaults::{ident, mk, pat};
use super::FnCx;
use crate::hir;

/// The elements of the array literal `e` (in parentheses, or `as const`), if it has neither a
/// spread nor a hole.
fn literal_elems(e: &Expr) -> Option<&[Expr]> {
    match &e.kind {
        E::Paren(inner) => literal_elems(inner),
        E::Cast { expr, ty } if super::expr::member::is_as_const(ty) => literal_elems(expr),
        E::Array(items)
            if !items.iter().any(|x| {
                matches!(&x.kind, E::Spread(_))
                    || matches!(&x.kind, E::Ident(i) if i.name == velt_syntax::ARRAY_HOLE)
            }) =>
        {
            Some(items)
        }
        _ => None,
    }
}

/// Are `items` literals of one type (`[10, 20]`, `["a", "b"]`)? Their element type is
/// already that type, so the pattern takes the array apart as before.
fn same_kind_literals(items: &[Expr]) -> bool {
    let kind = |x: &Expr| match &x.kind {
        E::Lit(ast::Lit::Int { suffix: None, .. } | ast::Lit::Float { suffix: None, .. }) => {
            Some(0)
        }
        E::Lit(ast::Lit::Str(_)) => Some(1),
        E::Lit(ast::Lit::Bool(_)) => Some(2),
        _ => None,
    };
    let first = items.first().and_then(kind);
    first.is_some() && items.iter().all(|x| kind(x) == first)
}

impl FnCx<'_, '_> {
    /// Evaluate the elements `items` into hidden temporaries, in order, and list each pattern
    /// of `elems` with the temporary it takes apart (`binds`). A nested array pattern without a
    /// default or rest over a nested literal is taken apart the same way, so its elements keep
    /// their types too.
    fn literal_temps(
        &mut self,
        elems: &[ast::Pattern],
        items: &[Expr],
        binds: &mut Vec<(ast::Pattern, Expr)>,
        out: &mut Vec<hir::Stmt>,
    ) {
        for (k, x) in items.iter().enumerate() {
            let p = elems.get(k);
            if let Some(ast::Pattern {
                kind:
                    P::Array {
                        elems: inner,
                        rest: None,
                    },
                ..
            }) = p
            {
                if let Some(nested) = literal_elems(x).filter(|n| n.len() >= inner.len()) {
                    self.literal_temps(inner, nested, binds, out);
                    continue;
                }
            }
            let span = x.span;
            let name = ident(&format!("#d{}", self.f.locals.len()), span);
            let d = VarDecl {
                kind: VarKind::Const,
                pattern: pat(P::Ident(name.clone()), span),
                ty: None,
                init: Some(x.clone()),
                span,
            };
            self.var_decl(&d, span, out);
            if let Some(p) = p.filter(|p| !matches!(p.kind, P::Wildcard)) {
                binds.push((p.clone(), mk(E::Ident(name), span)));
            }
        }
    }

    /// `kind [p1, p2] = [e1, e2, …]` without a type annotation, element by element (module
    /// docs); `false` (nothing done) for any other declaration.
    pub(super) fn literal_pattern_decl(&mut self, v: &VarDecl, out: &mut Vec<hir::Stmt>) -> bool {
        let (P::Array { elems, rest: None }, None, Some(init)) = (&v.pattern.kind, &v.ty, &v.init)
        else {
            return false;
        };
        let tuple_reads;
        let items = match literal_elems(init) {
            Some(items) => items,
            // `const c = [2, "y"] as const; [b, t] = c;`: `c[0]` as a `number`, `c[1]` as a
            // `string`, each read only as far as the pattern goes.
            None => match self.as_const_reads(init) {
                Some(reads) if reads.len() >= elems.len() => {
                    tuple_reads = reads;
                    &tuple_reads[..elems.len()]
                }
                _ => return false,
            },
        };
        if items.len() < elems.len() || same_kind_literals(items) {
            return false;
        }
        let mut binds = vec![];
        self.literal_temps(elems, items, &mut binds, out);
        for (p, t) in binds {
            self.decl_with_defaults(v.kind, &p, t, out);
        }
        true
    }
}

impl FnCx<'_, '_> {
    /// `kind pattern = tmp` where the hidden temporary (or variable) `tmp` holds an `any`
    /// (module docs).
    pub(super) fn any_pattern_decls(
        &mut self,
        kind: VarKind,
        pattern: &ast::Pattern,
        tmp: &Expr,
        out: &mut Vec<hir::Stmt>,
    ) {
        self.read_pattern_decls(kind, pattern, tmp, true, out);
    }

    /// `kind pattern = tmp`, each element or property read from `tmp` (an index, a member, or
    /// `tmp[k]` for a computed key); `dynamic`: `tmp` is an `any` (module docs).
    fn read_pattern_decls(
        &mut self,
        kind: VarKind,
        pattern: &ast::Pattern,
        tmp: &Expr,
        dynamic: bool,
        out: &mut Vec<hir::Stmt>,
    ) {
        let reads: Vec<(&ast::Pattern, Expr)> = match &pattern.kind {
            P::Array { elems, rest } => {
                if let Some(r) = rest {
                    let msg = "`...rest` of an `any` value is not supported yet; give the value an array type first";
                    self.unsupported_rest(kind, r, msg, out);
                }
                elems
                    .iter()
                    .enumerate()
                    .filter(|(_, p)| !matches!(p.kind, P::Wildcard))
                    .map(|(k, p)| {
                        let at = mk(
                            E::Lit(ast::Lit::Int {
                                value: k as u128,
                                suffix: None,
                            }),
                            p.span,
                        );
                        let read = E::Index {
                            object: Box::new(tmp.clone()),
                            index: Box::new(at),
                            optional: false,
                        };
                        (p, mk(read, p.span))
                    })
                    .collect()
            }
            P::Object { fields, rest } => {
                if let Some(r) = rest {
                    let msg = match dynamic {
                        true => "`...rest` of an `any` value is not supported yet; give the value an object type first",
                        false => "`...rest` next to a computed key is not supported yet",
                    };
                    self.unsupported_rest(kind, r, msg, out);
                }
                let mut reads = vec![];
                for (key, p) in fields {
                    let read = match self.key_read(key) {
                        KeyRead::Index(k) => E::Index {
                            object: Box::new(tmp.clone()),
                            index: Box::new(mk(E::Ident(k), key.span)),
                            optional: false,
                        },
                        KeyRead::Member(k) => E::Member {
                            object: Box::new(tmp.clone()),
                            prop: k,
                            optional: false,
                        },
                        KeyRead::Reported => E::Ident(ident(crate::body::REPORTED, key.span)),
                    };
                    reads.push((p, mk(read, key.span)));
                }
                reads
            }
            _ => return,
        };
        for (p, read) in reads {
            let (p, value) = match &p.kind {
                P::Default { pattern, value } if !dynamic => {
                    let span = read.span;
                    let callee = mk(E::Ident(ident(crate::body::UNDEFINED_OR, span)), span);
                    let call = E::Call {
                        callee: Box::new(callee),
                        type_args: vec![],
                        args: vec![read, (**value).clone()],
                        optional: false,
                    };
                    (&**pattern, mk(call, span))
                }
                P::Default { pattern, value }
                    if !matches!(value.kind, E::Lit(ast::Lit::Undefined)) =>
                {
                    let span = read.span;
                    // Read once (a getter runs once, as in JavaScript).
                    let name = ident(&format!("#d{}", self.f.locals.len()), span);
                    let d = VarDecl {
                        kind: VarKind::Const,
                        pattern: pat(P::Ident(name.clone()), span),
                        ty: None,
                        init: Some(read),
                        span,
                    };
                    self.var_decl(&d, span, out);
                    let read = mk(E::Ident(name), span);
                    let test = mk(
                        E::Binary {
                            op: ast::BinaryOp::StrictEq,
                            lhs: Box::new(read.clone()),
                            rhs: Box::new(mk(E::Lit(ast::Lit::Undefined), span)),
                        },
                        span,
                    );
                    let d = mk(
                        E::Cast {
                            expr: value.clone(),
                            ty: ast::TypeExpr {
                                kind: ast::TypeExprKind::Named {
                                    path: vec![ident("any", span)],
                                    args: vec![],
                                },
                                span,
                            },
                        },
                        span,
                    );
                    let cond = E::Cond {
                        cond: Box::new(test),
                        then: Box::new(d),
                        els: Box::new(read),
                    };
                    (&**pattern, mk(cond, span))
                }
                P::Default { pattern, .. } => (&**pattern, read),
                _ => (p, read),
            };
            self.decl_with_defaults(kind, p, value, out);
        }
    }
}

/// How an object pattern reads the property its key names.
enum KeyRead {
    /// `tmp.name` (a plain key, or a computed key naming a module constant).
    Member(ast::Ident),
    /// `tmp[k]` (a computed key naming a variable).
    Index(ast::Ident),
    /// Already reported: the binding is an error.
    Reported,
}

impl FnCx<'_, '_> {
    /// The reads of the elements of the literal of literals of different types `as const` that
    /// the `const` variable `c` holds (`Frame::as_const_tuples`): a local of this function or an
    /// enclosing one, or a module constant. Each element is read from the array, as its
    /// literal's type (`c[0] as unknown as number`): the array may have been written through a
    /// cast, so a value that is no longer of that type stops with a `TypeError`.
    fn as_const_reads(&mut self, init: &Expr) -> Option<Vec<Expr>> {
        let mut e = init;
        while let E::Paren(x) = &e.kind {
            e = x;
        }
        let E::Ident(name) = &e.kind else {
            return None;
        };
        let elems = match self.local_as_const_tuple(&name.name) {
            Some(local) => local?.to_vec(),
            None => {
                let d = match self.cx.lookup_item(self.module, &name.name) {
                    Some(crate::ctx::Item::Def(d)) => d,
                    _ => return None,
                };
                let init = self.cx.global(d).filter(|g| !g.mutable)?.src.init?;
                match &init.kind {
                    E::Cast { expr, ty } if super::expr::member::is_as_const(ty) => {
                        super::readonly_arrays::as_const_tuple(expr)?
                    }
                    _ => return None,
                }
            }
        };
        let span = e.span;
        let named = |n: &str| ast::TypeExpr {
            kind: ast::TypeExprKind::Named {
                path: vec![ident(n, span)],
                args: vec![],
            },
            span,
        };
        let cast = |expr: Expr, ty: ast::TypeExpr| {
            mk(
                E::Cast {
                    expr: Box::new(expr),
                    ty,
                },
                span,
            )
        };
        elems
            .iter()
            .enumerate()
            .map(|(k, x)| {
                let ty = super::readonly_arrays::literal_type(x)?;
                let read = mk(
                    E::Index {
                        object: Box::new(e.clone()),
                        index: Box::new(mk(
                            E::Lit(ast::Lit::Int {
                                value: k as u128,
                                suffix: None,
                            }),
                            span,
                        )),
                        optional: false,
                    },
                    span,
                );
                Some(cast(cast(read, named("unknown")), named(ty)))
            })
            .collect()
    }

    /// How the object pattern key `key` reads its property: a computed key `[k]` naming a
    /// variable (a local, an outer function's, a module variable that is not a key the
    /// compiler knows) reads `tmp[k]`; one naming a module constant holding a string or a
    /// symbol reads that member.
    fn key_read(&mut self, key: &ast::Ident) -> KeyRead {
        if key.name == velt_syntax::REPORTED_KEY {
            return KeyRead::Reported;
        }
        if !crate::symbols::is_computed_key(key) {
            return KeyRead::Member(key.clone());
        }
        let inner = key.name[1..key.name.len() - 1].to_string();
        if self.local_hides(&inner) {
            return KeyRead::Index(ident(&inner, key.span));
        }
        let global = matches!(self.cx.lookup_item(self.module, &inner),
            Some(crate::ctx::Item::Def(d)) if matches!(self.cx.info[d.0 as usize], crate::defs::DefInfo::Global(_)));
        match self.cx.resolve_key(self.module, key) {
            Ok(name) => KeyRead::Member(ident(&name, key.span)),
            Err(_) if global => KeyRead::Index(ident(&inner, key.span)),
            Err(_) => {
                self.cx.bad_key(&key.name, &inner, key.span);
                KeyRead::Reported
            }
        }
    }

    /// Is `key` a computed key (or one the parser reported)?
    fn computed(key: &ast::Ident) -> bool {
        key.name == velt_syntax::REPORTED_KEY || crate::symbols::is_computed_key(key)
    }

    /// `...rest` that is not supported here: reported, and declared as an error so its uses say
    /// nothing more.
    pub(super) fn unsupported_rest(
        &mut self,
        kind: VarKind,
        r: &ast::Ident,
        msg: &str,
        out: &mut Vec<hir::Stmt>,
    ) {
        self.cx.err(msg, r.span);
        let reported = mk(E::Ident(ident(crate::body::REPORTED, r.span)), r.span);
        self.decl_with_defaults(kind, &pat(P::Ident(r.clone()), r.span), reported, out);
    }

    /// `kind { [k]: v, … } = init` with a computed key (module docs); `false` (nothing done)
    /// for any other declaration.
    pub(super) fn computed_key_decl(&mut self, v: &VarDecl, out: &mut Vec<hir::Stmt>) -> bool {
        let (P::Object { fields, .. }, Some(init)) = (&v.pattern.kind, &v.init) else {
            return false;
        };
        if !fields.iter().any(|(k, _)| Self::computed(k)) {
            return false;
        }
        let span = init.span;
        let name = ident(&format!("#d{}", self.f.locals.len()), span);
        let d = VarDecl {
            kind: VarKind::Const,
            pattern: pat(P::Ident(name.clone()), span),
            ty: v.ty.clone(),
            init: Some(init.clone()),
            span,
        };
        self.var_decl(&d, span, out);
        let tmp = mk(E::Ident(name), span);
        self.read_pattern_decls(v.kind, &v.pattern, &tmp, false, out);
        true
    }
}
