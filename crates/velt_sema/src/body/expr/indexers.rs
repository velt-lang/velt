//! Indexing a type that defines the operator method `[Symbol.index](i)`, and
//! `[Symbol.setIndex](i, v)` for writes (design operators.md, #2050): a user class, struct or
//! vector, and std's typed arrays (`Uint8Array`, `Float64Array`, …,
//! std/lang/typed_array/index.vlt), whose elements live in an `ArrayBuffer` rather than in a
//! Velt array, and `RegExpMatch`. `xs[i]` reads `xs[Symbol.index](i)`; `xs[i] = v` is
//! `xs[Symbol.setIndex](i, v)`, and `xs[i] op= v` (in place on the element with its
//! `[Symbol.opAssign]`), `xs[i]++` and `xs[i] ??= v` read, then write, as in JS:
//!
//! ```text
//! { const <receiver> = xs;           // only when evaluating `xs` may have an effect
//!   const <index> = i;               // only when `i` is not a literal
//!   const <old> = <receiver>[Symbol.index](<index>);
//!   const <new> = <old> op v;
//!   <receiver>[Symbol.setIndex](<index>, <new>);
//!   <old> or <new> }                 // the value, where one is wanted
//! ```
//!
//! A key naming a member stays property access, as in TypeScript: `a["x"]` with a member `x`
//! (or a key of a literal type naming one) is the member, and the indexer takes the rest.
//!
//! `m[i, j]` passes several keys (`m[Symbol.index](i, j)`, `m[Symbol.setIndex](i, j, v)`): the
//! object, then each key once, left to right, then the value; one temporary per key.

use velt_common::Span;
use velt_syntax::ast;

use super::accessor_rmw::{binary, block, is_field_path, stmt, Rmw};
use super::assign_paths::is_pure_value;
use super::setters::synth;
use crate::body::{FnCx, Want};
use crate::hir::{self, ExprKind as H, Nothing, TyId};

use velt_syntax::operators::{INDEX, SET_INDEX};
/// An array's checked read (`T | undefined`), std/prelude/array.vlt.
const GET: &str = velt_common::std_key!(array_get);
const INDEX_TEMP: &str = "<index>";
const OLD: &str = "<old>";
const NEW: &str = "<new>";

fn ident(name: &str, span: Span) -> ast::Ident {
    ast::Ident {
        name: name.into(),
        span,
    }
}

fn name_ref(name: &str, span: Span) -> ast::Expr {
    synth(ast::ExprKind::Ident(ident(name, span)), span)
}

/// `(object, keys)` of `object[index]` or `object[index, more…]` (parentheses removed).
fn index_parts(target: &ast::Expr) -> Option<(&ast::Expr, Vec<&ast::Expr>)> {
    match &target.kind {
        ast::ExprKind::Paren(inner) => index_parts(inner),
        ast::ExprKind::Index {
            object,
            index,
            more,
            optional: false,
        } => Some((object, keys_of(index, more))),
        _ => None,
    }
}

/// The keys of `[index, more…]`.
pub(super) fn keys_of<'a>(index: &'a ast::Expr, more: &'a [ast::Expr]) -> Vec<&'a ast::Expr> {
    std::iter::once(index).chain(more).collect()
}

/// The temporary holding key `n` (`<index>`, `<index1>`, …).
fn key_temp(n: usize) -> String {
    match n {
        0 => INDEX_TEMP.to_string(),
        n => format!("<index{n}>"),
    }
}

impl FnCx<'_, '_> {
    /// Does `t` (a class, struct, vector, interface value or type parameter) define
    /// `[Symbol.index]`?
    pub(crate) fn has_indexer(&mut self, t: TyId) -> bool {
        self.has_instance_op(t, INDEX)
    }

    /// Is `obj[index]` (`obj` of type `t`) read and written through `t`'s indexer: `t` has one,
    /// and the key is no symbol key and names no member (`a["x"]` with a member `x` is the
    /// member, as in TypeScript)?
    pub(crate) fn indexes_through(&mut self, t: TyId, index: &ast::Expr) -> bool {
        if !self.has_indexer(t) || self.symbol_key_of(index).is_some() {
            return false;
        }
        match self.single_key(index) {
            Some(k) => {
                !(self.field_of(t, &k).is_some()
                    || self.has_getter(t, &k)
                    || self.has_setter(t, &k)
                    || self.method_exists(t, &k))
            }
            None => true,
        }
    }

    /// The type of parameter `pos` of `t`'s method `method` (an indexer's key) at `t`'s type
    /// arguments (`i: K` of `Grid<K>` is `usize` on a `Grid<usize>`), when it is known.
    fn index_param(&mut self, t: TyId, method: &str, pos: usize) -> Option<TyId> {
        use super::method::Resolved;
        let (p, args) = match self.resolve_method(t, method)? {
            Resolved::Def { def, slots, .. } | Resolved::Virtual { def, slots, .. } => {
                let p = self.cx.fn_info(def).params.get(pos)?.ty;
                (p, slots.into_iter().collect::<Option<Vec<_>>>())
            }
            Resolved::Iface {
                method, iface_args, ..
            } => (method.params.get(pos)?.ty, Some(iface_args)),
            Resolved::Builtin(_) => return None,
        };
        let p = match args {
            Some(args) if !args.is_empty() => self.cx.subst(p, &args),
            _ => p,
        };
        (!super::iface_adapt::has_params(self.cx, p)).then_some(p)
    }

    /// `obj[index]` read through `[INDEX]`.
    pub(super) fn indexer_read(
        &mut self,
        obj: hir::Expr,
        index: &ast::Expr,
        span: Span,
    ) -> hir::Expr {
        self.read_through(obj, &[index], INDEX, span)
    }

    /// `obj[i, j, …]` read through `[INDEX]`.
    pub(super) fn indexer_read_keys(
        &mut self,
        obj: hir::Expr,
        keys: &[&ast::Expr],
        span: Span,
    ) -> hir::Expr {
        self.read_through(obj, keys, INDEX, span)
    }

    /// `xs[i]` on an array where the read is expected to be `T | undefined` (`return xs[0]`
    /// from a function returning `T | undefined`, `const v: T | undefined = xs[i]`): a checked
    /// read, `xs[ARRAY_GET](i)`, which gives `undefined` for a position outside the array, as in
    /// JS (`T | undefined` at `T = U | undefined` is `U | undefined` itself). A read typed `T`
    /// stays the plain index (an out-of-range one panics); `Err` gives the object back for it.
    pub(super) fn checked_array_read(
        &mut self,
        obj: hir::Expr,
        index: &ast::Expr,
        exp: Option<TyId>,
        want: Want,
        span: Span,
    ) -> Result<hir::Expr, hir::Expr> {
        let Some(elem) = self.cx.ty.array_elem(obj.ty) else {
            return Err(obj);
        };
        let allows_undefined = |s: &Self, t: TyId| {
            matches!(
                s.cx.ty.nothing_of(t),
                Some(Nothing::Undefined | Nothing::Both)
            )
        };
        // Expected as `T | undefined` of the element type itself (not a type a generic call
        // is still inferring), or an element that may itself be `undefined`
        // (`(T | undefined)[]`), which is read this way wherever it is used.
        // `T | null | undefined` of a `(T | null)[]` too: its payload is the element type.
        let expected = exp.is_some_and(|t| match self.cx.ty.nothing_of(t) {
            Some(Nothing::Undefined) => self.cx.ty.core(t) == Some(elem),
            Some(Nothing::Both) => {
                self.cx.ty.opt_payload(t) == Some(elem) || self.cx.ty.core(t) == Some(elem)
            }
            _ => false,
        });
        let wanted = expected || allows_undefined(self, elem);
        // The standard library reads its arrays as it states (its code is not TypeScript's).
        if !wanted
            || self.cx.scopes[self.module].is_std
            || want == Want::BorrowMut
            || self.cx.ty.is_bottom(elem)
            || self
                .peek_ty(index)
                .is_some_and(|t| !self.cx.ty.is_numeric(t))
        {
            return Err(obj);
        }
        Ok(self.read_through(obj, &[index], GET, span))
    }

    /// `obj.<method>(keys…)`, each key evaluated once, after the object.
    fn read_through(
        &mut self,
        obj: hir::Expr,
        keys: &[&ast::Expr],
        method: &str,
        span: Span,
    ) -> hir::Expr {
        // The method is named by the brackets (`[0]` of `a[0]`: hover and go-to-definition).
        let at = match obj.span.file == span.file && obj.span.hi < span.hi {
            true => Span {
                lo: obj.span.hi,
                ..span
            },
            false => span,
        };
        let prop = ident(method, at);
        if let [index] = keys {
            let param = self.index_param(obj.ty, method, 0);
            if self.number_index(index, param) {
                return self.method_call_on(obj, &prop, &[], &[(*index).clone()], None, span);
            }
        }
        // An integer or computed index: evaluated once, after the object.
        let mut stmts = vec![];
        self.push_scope();
        let recv = self.object_once(obj, &mut stmts);
        let idx = self.keys_once(recv.ty, method, keys, &mut stmts);
        let call = self.method_call_on(recv, &prop, &[], &idx, None, span);
        self.pop_scope();
        self.temps_then(stmts, call, span)
    }

    /// `object[index] = value` (`op` None) or `object[index] op= value` on an indexer class;
    /// `obj` is the checked object. `as_value`: the assignment's value is used.
    pub(super) fn indexer_assign(
        &mut self,
        obj: hir::Expr,
        op: Option<ast::BinaryOp>,
        target: &ast::Expr,
        value: &ast::Expr,
        as_value: bool,
        span: Span,
    ) -> hir::Expr {
        let Some((object, keys)) = index_parts(target) else {
            return self.error_expr(span);
        };
        if !self.has_instance_op(obj.ty, SET_INDEX) {
            return self.read_only_indexer(&obj, &keys, value, span);
        }
        let param = self.index_param(obj.ty, SET_INDEX, 0);
        let one = match keys[..] {
            [index] => self.number_index(index, param),
            _ => false,
        };
        match op {
            // `xs[i] = v` as a statement: one call, its arguments evaluated in JS's order.
            None if !as_value && one && is_pure_value(value) => {
                let prop = ident(SET_INDEX, span);
                let args = [ast::Expr::clone(keys[0]), value.clone()];
                self.method_call_on(obj, &prop, &[], &args, None, span)
            }
            // Otherwise the object, the index and the value are evaluated first, in that order
            // (`u[0] = u[1] = 7`: the value may use the object itself).
            None if !as_value => {
                let mut stmts = vec![];
                self.push_scope();
                let recv = self.receiver_once(obj, object, Some(value), &mut stmts);
                let mut args = self.keys_once(recv.ty, SET_INDEX, &keys, &mut stmts);
                let v = self.expr(value, None, Want::Move);
                self.bind_temp(NEW, v, &mut stmts);
                args.push(name_ref(NEW, span));
                let call =
                    self.method_call_on(recv, &ident(SET_INDEX, span), &[], &args, None, span);
                self.pop_scope();
                self.temps_then(stmts, call, span)
            }
            None => self.indexer_assign_value(obj, object, &keys, value, span),
            Some(op @ (ast::BinaryOp::And | ast::BinaryOp::Or | ast::BinaryOp::Nullish)) => {
                self.indexer_rmw(obj, object, &keys, Rmw::Logical(op, value), as_value, span)
            }
            Some(op) => {
                self.indexer_rmw(obj, object, &keys, Rmw::Compound(op, value), as_value, span)
            }
        }
    }

    /// `object[index]++` / `--object[index]` on an indexer class.
    pub(super) fn indexer_update(
        &mut self,
        obj: hir::Expr,
        op: ast::UpdateOp,
        prefix: bool,
        target: &ast::Expr,
        as_value: bool,
        span: Span,
    ) -> hir::Expr {
        let Some((object, keys)) = index_parts(target) else {
            return self.error_expr(span);
        };
        self.indexer_rmw(obj, object, &keys, Rmw::Update(op, prefix), as_value, span)
    }

    /// The block of the module comment.
    fn indexer_rmw(
        &mut self,
        obj: hir::Expr,
        object: &ast::Expr,
        keys: &[&ast::Expr],
        rmw: Rmw,
        as_value: bool,
        span: Span,
    ) -> hir::Expr {
        let mut stmts = vec![];
        self.push_scope();
        let later = match rmw {
            Rmw::Compound(_, v) | Rmw::Logical(_, v) => Some(v),
            Rmw::Update(..) => None,
        };
        let recv = self.receiver_once(obj, object, later, &mut stmts);
        let idx = self.keys_once(recv.ty, INDEX, keys, &mut stmts);
        let e = self.indexer_rmw_in(recv, idx, rmw, as_value, span, &mut stmts);
        self.pop_scope();
        e
    }

    fn indexer_rmw_in(
        &mut self,
        recv: hir::Expr,
        idx: Vec<ast::Expr>,
        rmw: Rmw,
        as_value: bool,
        span: Span,
        stmts: &mut Vec<hir::Stmt>,
    ) -> hir::Expr {
        let unit = self.cx.ty.unit;
        let set_prop = ident(SET_INDEX, span);
        let keys: Vec<&ast::Expr> = idx.iter().collect();
        let old_val = self.indexer_read_keys(recv.clone(), &keys, span);
        let old_ty = old_val.ty;
        if self.cx.ty.is_bottom(old_ty) {
            return self.error_expr(span);
        }
        let old = self.bind_temp(OLD, old_val, stmts);
        let mut computed = None;
        let (value, cond) = match rmw {
            Rmw::Compound(op, v) => {
                let new = self.rmw_compound_value(op, (old, OLD), old_ty, v, span);
                computed = Some(new);
                (name_ref(OLD, span), None)
            }
            Rmw::Update(op, _) => {
                if !self.cx.ty.is_numeric(old_ty) && !self.operator_type(old_ty) {
                    let opname = if op == ast::UpdateOp::Inc { "++" } else { "--" };
                    let tn = self.cx.display(old_ty);
                    self.cx
                        .err(format!("cannot apply `{opname}` to type `{tn}`"), span);
                    return self.error_expr(span);
                }
                let one = synth(
                    ast::ExprKind::Lit(ast::Lit::Int {
                        value: 1,
                        suffix: None,
                    }),
                    span,
                );
                let op = match op {
                    ast::UpdateOp::Inc => ast::BinaryOp::Add,
                    ast::UpdateOp::Dec => ast::BinaryOp::Sub,
                };
                (binary(op, name_ref(OLD, span), one, span), None)
            }
            Rmw::Logical(_, _) if as_value => {
                self.cx.err(
                    "`??=`, `||=` and `&&=` on an element of a typed array are not supported as a value yet",
                    span,
                );
                return self.error_expr(span);
            }
            Rmw::Logical(op, v) => (v.clone(), Some(self.decides(op, name_ref(OLD, span), span))),
        };
        let mut set = vec![];
        let new_val = match computed {
            Some(new) => new,
            None => self.expr(&value, Some(old_ty), Want::Move),
        };
        let new_ty = new_val.ty;
        let new = self.bind_temp(NEW, new_val, &mut set);
        let mut args = idx;
        args.push(name_ref(NEW, span));
        let call = self.method_call_on(recv, &set_prop, &[], &args, None, span);
        set.push(stmt(hir::StmtKind::Expr(call), span));
        match cond {
            // The write happens only when the old value does not decide the result.
            Some(cond) => {
                let then = self.mk(H::Block(block(set, None, span)), unit, span);
                let els = self.mk(H::Block(block(vec![], None, span)), unit, span);
                let skip = H::If {
                    cond: Box::new(cond),
                    then: Box::new(then),
                    els: Box::new(els),
                };
                let skip = self.mk(skip, unit, span);
                stmts.push(stmt(hir::StmtKind::Expr(skip), span));
            }
            None => stmts.extend(set),
        }
        let value = match rmw {
            _ if !as_value => None,
            Rmw::Update(_, false) => Some(self.read_temp(old, old_ty, span)),
            _ => Some(self.read_temp(new, new_ty, span)),
        };
        let ty = value.as_ref().map_or(unit, |v| v.ty);
        let b = block(std::mem::take(stmts), value, span);
        self.mk(H::Block(b), ty, span)
    }

    /// `(xs[i] = v)` used as a value on an indexer class: `v`, written through `[SET_INDEX]`
    /// after the object and index are evaluated.
    fn indexer_assign_value(
        &mut self,
        obj: hir::Expr,
        object: &ast::Expr,
        keys: &[&ast::Expr],
        value: &ast::Expr,
        span: Span,
    ) -> hir::Expr {
        let mut stmts = vec![];
        self.push_scope();
        let recv = self.receiver_once(obj, object, Some(value), &mut stmts);
        let mut args = self.keys_once(recv.ty, SET_INDEX, keys, &mut stmts);
        let v = self.expr(value, None, Want::Move);
        let ty = v.ty;
        let new = self.bind_temp(NEW, v, &mut stmts);
        args.push(name_ref(NEW, span));
        let call = self.method_call_on(recv, &ident(SET_INDEX, span), &[], &args, None, span);
        stmts.push(stmt(hir::StmtKind::Expr(call), span));
        let value = self.read_temp(new, ty, span);
        self.pop_scope();
        self.mk(H::Block(block(stmts, Some(value), span)), ty, span)
    }

    /// `index` to evaluate once: a literal itself, else a temporary holding it. For an indexer
    /// taking a `number` (`param`), an integer is converted (`xs[k]` with `k: usize`, as for an
    /// array index).
    fn index_once(
        &mut self,
        index: &ast::Expr,
        param: Option<TyId>,
        n: usize,
        stmts: &mut Vec<hir::Stmt>,
    ) -> ast::Expr {
        if let ast::ExprKind::Lit(_) = &index.kind {
            return index.clone();
        }
        let f64_ = self.cx.ty.f64;
        let mut i = self.expr(index, param, Want::Move);
        if self.cx.ty.is_int(i.ty) && param.is_none_or(|p| p == f64_) {
            let (f64_, span) = (self.cx.ty.f64, i.span);
            i = self.mk(H::Cast(Box::new(i)), f64_, span);
        }
        let name = key_temp(n);
        self.bind_temp(&name, i, stmts);
        name_ref(&name, index.span)
    }

    /// Each key of an indexer call on a value of type `t` (`method` gives the keys' types),
    /// evaluated once, left to right ([`Self::index_once`]).
    fn keys_once(
        &mut self,
        t: TyId,
        method: &str,
        keys: &[&ast::Expr],
        stmts: &mut Vec<hir::Stmt>,
    ) -> Vec<ast::Expr> {
        let mut out = Vec::with_capacity(keys.len());
        for (n, k) in keys.iter().enumerate() {
            let param = self.index_param(t, method, n);
            out.push(self.index_once(k, param, n, stmts));
        }
        out
    }

    /// Is `index` passed to the indexer as it is: a literal, or a variable or field path that
    /// holds the indexer's index type (`param`, a number when unknown)? An integer one for a
    /// `number` index is converted, and any other index is evaluated once, after the object
    /// ([`Self::index_once`]).
    fn number_index(&mut self, index: &ast::Expr, param: Option<TyId>) -> bool {
        if let ast::ExprKind::Lit(_) = &index.kind {
            return true;
        }
        let want = param.unwrap_or(self.cx.ty.f64);
        self.peek_ty(index).is_some_and(|t| t == want)
    }

    /// `obj[index] = value` (or `op=`) on a type with `[Symbol.index]` and no
    /// `[Symbol.setIndex]`.
    fn read_only_indexer(
        &mut self,
        obj: &hir::Expr,
        keys: &[&ast::Expr],
        value: &ast::Expr,
        span: Span,
    ) -> hir::Expr {
        for k in keys {
            self.expr(k, None, Want::Borrow);
        }
        self.expr(value, None, Want::Borrow);
        let tn = self.cx.display(obj.ty);
        let d = match self.cx.ty.kind(obj.ty) {
            crate::hir::TyKind::Simd(_) => velt_common::Diagnostic::error(
                format!("cannot assign to a lane of `{tn}`: a vector is a value"),
                span,
            )
            .with_note("write `v = v.withLane(i, x)`"),
            _ => velt_common::Diagnostic::error(
                format!("cannot assign through `[]`: `{tn}` has `{INDEX}` but no `{SET_INDEX}`"),
                span,
            ),
        };
        self.cx.error(d);
        self.error_expr(span)
    }

    /// The object, evaluated once: itself when it is a variable or a path of fields, else a
    /// temporary holding it.
    fn object_once(&mut self, mut obj: hir::Expr, stmts: &mut Vec<hir::Stmt>) -> hir::Expr {
        if is_field_path(&obj) {
            return obj;
        }
        crate::body::places::set_place_mode(&mut obj, crate::hir::UseMode::Move);
        let (ty, span) = (obj.ty, obj.span);
        let l = self.bind_temp("<receiver>", obj, stmts);
        self.mk(H::Local(l, crate::hir::UseMode::Borrow), ty, span)
    }

    /// `{ stmts; value }`, or `value` alone without statements.
    fn temps_then(&mut self, stmts: Vec<hir::Stmt>, value: hir::Expr, span: Span) -> hir::Expr {
        if stmts.is_empty() {
            return value;
        }
        let ty = value.ty;
        self.mk(H::Block(block(stmts, Some(value), span)), ty, span)
    }
}
