//! A conditional spread of object literals, `{ a, ...(c ? { b: x } : {}) }`, the TypeScript
//! idiom for a key present only sometimes. TypeScript types the spread operand as
//! `{ b: number } | {}`, whose spread is `{ b?: number }`. Velt checks the conditional against
//! that object type (`cond_spread_hint`): a field that only some branches write is optional, so
//! each branch converts to it, and the spread copies the field only while present
//! (`spread.rs`), as JavaScript copies only the keys the chosen object has.

use velt_syntax::ast;

use crate::anon::ShapeField;
use crate::body::FnCx;
use crate::hir::TyId;

impl FnCx<'_, '_> {
    /// The object type a conditional spread operand `e` of object literals is checked as (see
    /// the module docs), or `None` when `e` is no such conditional, a branch does not check on
    /// its own, or the branches give one field different types.
    pub(super) fn cond_spread_hint(&mut self, e: &ast::Expr) -> Option<TyId> {
        self.cond_spread_hint_over(e, &[])
    }

    /// [`Self::cond_spread_hint`] in a literal whose earlier properties and spreads give the
    /// keys in `earlier` their types: a key the spread may override with another type has
    /// their union, as TypeScript types it (`{ k: false, ...(c ? { k: 1 } : {}) }` has `k:
    /// number | boolean`).
    pub(super) fn cond_spread_hint_over(
        &mut self,
        e: &ast::Expr,
        earlier: &[(String, TyId)],
    ) -> Option<TyId> {
        if !matches!(strip(e).kind, ast::ExprKind::Cond { .. }) {
            return None;
        }
        let mut out = self.cond_shape(e)?;
        for f in out.iter_mut() {
            let before = earlier.iter().rev().find(|(n, _)| *n == f.name);
            if let Some(&(_, t)) = before.filter(|(_, t)| *t != f.ty) {
                f.ty = self.cx.union_of(&[t, f.ty], false, e.span);
            }
        }
        Some(self.cx.anon_type_with(&out, self.module))
    }

    /// The fields of an object literal, or of a conditional of them (nested ones too), each
    /// optional when some branch does not write it. A `null` or `undefined` value is a field of
    /// that type alone, which joins the other branch's (`c ? { k: [1] } : { k: null }` has `k:
    /// number[] | null`).
    fn cond_shape(&mut self, e: &ast::Expr) -> Option<Vec<ShapeField>> {
        let e = strip(e);
        let (cond, then, els) = match &e.kind {
            ast::ExprKind::Object(props) if plain_props(props) => {
                let nothing = |v: &ast::Expr| match &v.kind {
                    ast::ExprKind::Lit(ast::Lit::Null) => Some(crate::hir::Nothing::Null),
                    ast::ExprKind::Lit(ast::Lit::Undefined) => Some(crate::hir::Nothing::Undefined),
                    _ => None,
                };
                let rest: Vec<ast::ObjectProp> = (props.iter())
                    .filter(
                        |p| !matches!(p, ast::ObjectProp::KeyValue(_, v) if nothing(v).is_some()),
                    )
                    .cloned()
                    .collect();
                let typed = ast::Expr {
                    id: ast::NodeId(u32::MAX),
                    kind: ast::ExprKind::Object(rest),
                    span: e.span,
                };
                let t = self.trial_type(&typed)?;
                let (d, args) = self.adt_of(t)?;
                let fields = self.cx.adt(d)?.fields.clone();
                let mut own = vec![];
                for p in props {
                    let (ast::ObjectProp::KeyValue(k, _) | ast::ObjectProp::Shorthand(k)) = p
                    else {
                        continue;
                    };
                    let name = self.literal_key_name(k.clone()).name;
                    if let ast::ObjectProp::KeyValue(_, v) = p {
                        if let Some(n) = nothing(v) {
                            let never = self.cx.ty.never;
                            let ty = self.cx.ty.option_of(never, n);
                            own.push(ShapeField::plain(&name, ty));
                            continue;
                        }
                    }
                    let f = fields.iter().find(|f| f.name == name)?;
                    own.push(ShapeField {
                        name,
                        ty: self.cx.subst(f.declared, &args),
                        readonly: false,
                        optional: f.optional,
                    });
                }
                return Some(own);
            }
            ast::ExprKind::Cond { cond, then, els } => (cond, then, els),
            _ => return None,
        };
        // Each branch under the narrowing of the condition, as `ternary` checks it
        // (`c ? { r: { ...c } } : {}` with `c` an object or `null`).
        let (when_true, when_false) = self.narrowing(cond);
        let mut shapes: Vec<Vec<ShapeField>> = vec![];
        for (b, facts) in [then, els].into_iter().zip([when_true, when_false]) {
            self.push_scope();
            facts.iter().for_each(|f| self.narrow(f));
            let shape = self.cond_shape(b);
            self.pop_scope();
            shapes.push(shape?);
        }
        // Fields in written order: the first branch's, then the second's others.
        let mut out: Vec<ShapeField> = vec![];
        for (k, shape) in shapes.iter().enumerate() {
            for f in shape {
                let other = shapes[1 - k].iter().find(|g| g.name == f.name);
                if let Some(seen) = out.iter_mut().find(|g| g.name == f.name) {
                    // Written with another type by the other branch: their union, as
                    // TypeScript types it (`c ? { k: 1 } : { k: "s" }` spreads `k: number |
                    // string`).
                    if seen.ty != f.ty {
                        let span = e.span;
                        seen.ty = self.cx.union_of(&[seen.ty, f.ty], false, span);
                    }
                    continue;
                }
                out.push(ShapeField {
                    optional: f.optional || other.is_none_or(|g| g.optional),
                    ..f.clone()
                });
            }
        }
        Some(out)
    }
}

/// Does a source read in spread value `v` declare `undefined` (a present field may hold it)?
pub(super) fn undefined_held(v: &super::spread::Value) -> bool {
    use super::spread::Value;
    match v {
        Value::Read(_, src) => src.undefined,
        Value::Over(_, src, prev) => src.undefined || undefined_held(prev),
        Value::Prop(..) | Value::Held(..) => true,
    }
}

impl FnCx<'_, '_> {
    /// Does the declared field type `t` (of a type with arguments `args`) hold `undefined`?
    pub(super) fn declares_undefined(&mut self, t: TyId, args: &[TyId]) -> bool {
        let t = self.cx.subst(t, args);
        self.cx.dynamic(t).is_some()
            || self.cx.has_type_params(t)
            || self.cx.ty.nothing_of(t).is_some_and(|n| n.has_undefined())
    }
}

/// Is `e` a conditional (in parentheses)?
pub(super) fn is_cond(e: &ast::Expr) -> bool {
    matches!(strip(e).kind, ast::ExprKind::Cond { .. })
}

impl FnCx<'_, '_> {
    /// The types of the keys an object literal's `entries` so far give, for a later
    /// conditional spread (`cond_spread_hint_over`): a property's value checked as a trial, a
    /// spread field's read (without the `undefined` an optional one may add).
    pub(super) fn entry_types(
        &mut self,
        entries: &[(String, super::spread::Value)],
    ) -> Vec<(String, TyId)> {
        use super::spread::Value;
        let mut out = vec![];
        for (name, v) in entries {
            let t = match v {
                Value::Read(h, src) | Value::Over(h, src, _) => match src.optional {
                    true => self.cx.ty.opt_payload(h.ty).unwrap_or(h.ty),
                    false => h.ty,
                },
                Value::Held(.., h) => h.ty,
                Value::Prop(k, value) => {
                    let read = ast::Expr {
                        id: ast::NodeId(u32::MAX),
                        kind: ast::ExprKind::Ident(k.clone()),
                        span: k.span,
                    };
                    let Some(t) = self.trial_type(value.unwrap_or(&read)) else {
                        continue;
                    };
                    self.cx.widened(t)
                }
            };
            out.push((name.clone(), t));
        }
        out
    }
}

/// A conditional spread operand whose object literals have getters (`c ? { get g() { return
/// 1; } } : {}`) with each getter called where it is written (`g: (() => { return 1; })()`):
/// a spread reads a getter's value, in property order, as JavaScript does. `None` when there
/// is none, a getter uses `this`, a value that is not a literal follows a getter, or the
/// literal has a setter (kept as written).
pub(super) fn called_getters(e: &ast::Expr) -> Option<ast::Expr> {
    if !matches!(strip(e).kind, ast::ExprKind::Cond { .. }) {
        return None;
    }
    let mut found = false;
    let out = call_getters(e, &mut found)?;
    found.then_some(out)
}

fn call_getters(e: &ast::Expr, found: &mut bool) -> Option<ast::Expr> {
    let e = strip(e);
    let kind = match &e.kind {
        ast::ExprKind::Cond { cond, then, els } => ast::ExprKind::Cond {
            cond: cond.clone(),
            then: Box::new(call_getters(then, found)?),
            els: Box::new(call_getters(els, found)?),
        },
        ast::ExprKind::Object(props) => {
            let mut out = vec![];
            let mut getter_seen = false;
            for p in props {
                let accessor = match p {
                    ast::ObjectProp::KeyValue(k, v) => match &v.kind {
                        ast::ExprKind::Call { callee, args, .. } => match (&callee.kind, &args[..])
                        {
                            (ast::ExprKind::Ident(f), [arrow]) if f.name == ast::OBJECT_GET_FN => {
                                Some((k, arrow, true))
                            }
                            (ast::ExprKind::Ident(f), _) if f.name == ast::OBJECT_SET_FN => {
                                return None;
                            }
                            _ => None,
                        },
                        _ => None,
                    },
                    _ => None,
                };
                let Some((k, arrow, _)) = accessor else {
                    // JavaScript builds the literal before the spread calls its getters: a
                    // value written after a getter runs first, which a getter called in place
                    // would not keep. Only a literal value may follow one.
                    let literal = matches!(p, ast::ObjectProp::KeyValue(_, v)
                        if matches!(v.kind, ast::ExprKind::Lit(_)));
                    if getter_seen && !literal {
                        return None;
                    }
                    out.push(p.clone());
                    continue;
                };
                // A getter using `this` reads the object the literal built, which a spread does
                // not keep: kept as written (an error).
                if uses_this(arrow) {
                    return None;
                }
                getter_seen = true;
                *found = true;
                let call = ast::ExprKind::Call {
                    callee: Box::new(ast::Expr {
                        id: ast::NodeId(u32::MAX),
                        kind: ast::ExprKind::Paren(Box::new(arrow.clone())),
                        span: arrow.span,
                    }),
                    type_args: vec![],
                    args: vec![],
                    optional: false,
                };
                let value = ast::Expr {
                    id: ast::NodeId(u32::MAX),
                    kind: call,
                    span: arrow.span,
                };
                out.push(ast::ObjectProp::KeyValue(k.clone(), value));
            }
            ast::ExprKind::Object(out)
        }
        _ => return Some(e.clone()),
    };
    Some(ast::Expr {
        id: e.id,
        kind,
        span: e.span,
    })
}

/// Does the getter function `f` use `this` (its object, which a spread does not pass)?
fn uses_this(f: &ast::Expr) -> bool {
    struct Find(bool);
    impl<'a> velt_syntax::visit::Visit<'a> for Find {
        fn expr(&mut self, e: &'a ast::Expr) {
            self.0 |= matches!(e.kind, ast::ExprKind::This);
        }
    }
    let mut v = Find(false);
    velt_syntax::visit::walk_expr(f, &mut v);
    v.0
}

fn strip(e: &ast::Expr) -> &ast::Expr {
    match &e.kind {
        ast::ExprKind::Paren(x) => strip(x),
        _ => e,
    }
}

/// Only `key: value` and shorthand properties (no spread, method or computed key, which have
/// their own rules).
fn plain_props(props: &[ast::ObjectProp]) -> bool {
    props.iter().all(|p| match p {
        ast::ObjectProp::KeyValue(k, _) => !crate::symbols::is_computed_key(k),
        ast::ObjectProp::Shorthand(_) => true,
        _ => false,
    })
}
