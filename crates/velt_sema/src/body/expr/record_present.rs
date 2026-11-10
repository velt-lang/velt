//! An open record's read used as its value type (#930 phase 3, option (c)). TypeScript types
//! `r[k]` of a `Record<string, V>` (or an index signature) as `V` and trusts that the key is
//! there; JavaScript gives `undefined` for a missing key and fails only where the program uses
//! that `undefined` as a `V`. Velt keeps the read a `V | undefined` underneath, so that
//! `=== undefined`, `??`, `?.` and storing it keep the `undefined`, and checks presence where
//! the read is used as a `V`:
//! - a member read or a method call (`r[k].value`, `r[k].f()`, `r[k][0]`): Node's `TypeError`,
//!   `Cannot read properties of undefined (reading 'value')`;
//! - a member write (`r[k].value = 1`): `Cannot set properties of undefined (setting 'value')`;
//! - a call of the value (`r[k]()`, `r.f()`): Node's `r[k] is not a function`;
//! - taking it apart (`const { a } = r[k]`): Node's `Cannot destructure property 'a' …`;
//! - a conversion to a `V` (an argument, a return value, a typed variable; `record_value_as`),
//!   and an operand of arithmetic or ordering, where Node goes on with `undefined` or `NaN`.
//!
//! A local declared without a type from such a read (`const e = r[k]`) keeps the same reads
//! (`trusted_locals`), and so does a parameter whose `any` default may be `undefined` (#2666).

use velt_common::Span;
use velt_syntax::ast;

use crate::body::FnCx;
use crate::hir::{self, Nothing, TyId};

impl FnCx<'_, '_> {
    /// The `V` that `h` is used as: `h` is an open record's read (or a trusted local's) of type
    /// `V | undefined`, where `V` holds no nothing of its own and is not dynamic.
    pub(super) fn trusted_value(&self, h: &hir::Expr) -> Option<TyId> {
        let v = *self.f.record_reads.get(&h.span)?;
        let fits = self.cx.ty.nothing_of(v).is_none()
            && self.cx.dynamic(v).is_none()
            && self.cx.ty.nothing_of(h.ty) == Some(Nothing::Undefined)
            && self.cx.ty.opt_payload(h.ty) == Some(v);
        fits.then_some(v)
    }

    /// `h` (the object of a member read or a method call `.prop`) as its `V`.
    pub(super) fn record_present_member(&mut self, h: hir::Expr, prop: &str) -> hir::Expr {
        if self.trusted_value(&h).is_none() {
            return h;
        }
        let msg = format!("TypeError: Cannot read properties of undefined (reading '{prop}')");
        self.record_present(h, &msg)
    }

    /// `h` (the object of `h[index]`) as its `V`. Node's message names the key's value: a
    /// literal key's is known here, a variable's is read when the check fails.
    pub(super) fn record_present_index(&mut self, h: hir::Expr, index: &ast::Expr) -> hir::Expr {
        let Some(v) = self.trusted_value(&h) else {
            return h;
        };
        let head = "TypeError: Cannot read properties of undefined (reading '";
        let mut inner = index;
        while let ast::ExprKind::Paren(x) = &inner.kind {
            inner = x;
        }
        let key = match &inner.kind {
            ast::ExprKind::Lit(ast::Lit::Str(s)) => Some(s.clone()),
            ast::ExprKind::Lit(ast::Lit::Int { value, .. }) => Some(value.to_string()),
            _ => None,
        };
        let span = h.span;
        let msg = match (key, &inner.kind) {
            (Some(k), _) => str_expr(&format!("{head}{k}')"), span),
            (None, ast::ExprKind::Ident(id)) => ast::Expr {
                id: ast::NodeId(u32::MAX),
                kind: ast::ExprKind::Template {
                    quasis: vec![head.to_string(), "')".to_string()],
                    exprs: vec![ast::Expr {
                        id: ast::NodeId(u32::MAX),
                        kind: ast::ExprKind::Ident(id.clone()),
                        span: inner.span,
                    }],
                },
                span,
            },
            _ => str_expr("TypeError: Cannot read properties of undefined", span),
        };
        self.checked_present_with(h, msg, Some(v), true, span)
    }

    /// `f`, the value called by `callee(…)`, as its `V`: Node's `TypeError` names the callee.
    pub(super) fn record_present_callee(&mut self, f: hir::Expr, callee: &ast::Expr) -> hir::Expr {
        if self.trusted_value(&f).is_none() {
            return f;
        }
        let msg = format!("TypeError: {} is not a function", node_text(callee));
        self.record_present(f, &msg)
    }

    /// `h`, an operand of an arithmetic, bitwise or ordering operator, as its `V`. A number's
    /// missing value is `NaN`, as JavaScript converts `undefined` (`r[k] + 1` is `NaN`, `~r[k]`
    /// is `-1`, `r[k] < 5` is `false`). Any other `V` stops, as at a conversion
    /// (`record_value_as`): `s[k] + 1` of a string can't hold Node's `NaN`.
    pub(super) fn record_operand(&mut self, h: hir::Expr) -> hir::Expr {
        let Some(v) = self.trusted_value(&h) else {
            return h;
        };
        if v != self.cx.ty.f64 {
            return self.record_present(h, "TypeError: the record has no value at this key");
        }
        let span = h.span;
        let nan = ast::Expr {
            id: ast::NodeId(u32::MAX),
            kind: ast::ExprKind::Ident(ast::Ident {
                name: "NaN".into(),
                span,
            }),
            span,
        };
        self.present_or_expr(h, &nan, Some(v), true, span)
    }

    /// `l op r` with `op` an ordering (`<`, `<=`, `>`, `>=`) where a side is a string's read:
    /// `false` when its value is missing, as `undefined < "c"` is in JavaScript. `Err` gives
    /// the operands back when neither is one, or when skipping the other side would skip what
    /// it does (it is not a variable, a literal or a read).
    pub(super) fn record_ordering(
        &mut self,
        op: ast::BinaryOp,
        l: hir::Expr,
        r: hir::Expr,
        span: Span,
    ) -> Result<hir::Expr, Box<(hir::Expr, hir::Expr)>> {
        let str_ = self.cx.ty.str_;
        let lt = self.trusted_value(&l) == Some(str_);
        let rt = self.trusted_value(&r) == Some(str_);
        let pure = |s: &Self, h: &hir::Expr| {
            matches!(h.kind, hir::ExprKind::Lit(_) | hir::ExprKind::Local(..))
                || s.trusted_value(h).is_some()
        };
        if !(lt || rt) || (lt && !pure(self, &r)) || (rt && !pure(self, &l)) {
            return Err(Box::new((l, r)));
        }
        let no = |s: &mut Self| {
            s.mk(
                hir::ExprKind::Lit(hir::Lit::Bool(false)),
                s.cx.ty.bool_,
                span,
            )
        };
        let none = no(self);
        Ok(match (lt, rt) {
            (true, true) => self.present_match(
                l,
                |s, lv| {
                    let none = no(s);
                    s.present_match(r, |s, rv| s.binary_checked(op, lv, rv, span), none, span)
                },
                none,
                span,
            ),
            (true, false) => {
                self.present_match(l, |s, lv| s.binary_checked(op, lv, r, span), none, span)
            }
            _ => self.present_match(r, |s, rv| s.binary_checked(op, l, rv, span), none, span),
        })
    }

    /// `h` iterated (`for…of`, a spread): Node's `TypeError: e is not iterable`, with `suffix`.
    pub(crate) fn record_present_iter(
        &mut self,
        h: hir::Expr,
        e: &ast::Expr,
        suffix: &str,
    ) -> hir::Expr {
        if self.trusted_value(&h).is_none() {
            return h;
        }
        let msg = format!(
            "TypeError: {} is not iterable{suffix}",
            node_text_as(e, false)
        );
        self.record_present(h, &msg)
    }

    /// `e`, taken apart by the pattern `p` of a declaration without a type: the payload of a
    /// narrowed value (`checked_copies`), and an open record's read as its `V`, checked with
    /// Node's `Cannot destructure property 'a' of 'e' as it is undefined.` (an array pattern:
    /// `undefined is not iterable …`).
    pub(crate) fn pattern_init(&mut self, e: &ast::Expr, p: &ast::Pattern) -> hir::Expr {
        let init = self.payload_expr(e, None, crate::body::Want::Borrow);
        if self.trusted_value(&init).is_none() {
            return init;
        }
        let text = node_text(e);
        let msg = match &p.kind {
            ast::PatternKind::Object { fields, .. } => match fields.first() {
                Some((name, _)) => format!(
                    "TypeError: Cannot destructure property '{}' of '{text}' as it is undefined.",
                    name.name
                ),
                None => format!("TypeError: Cannot destructure '{text}' as it is undefined."),
            },
            ast::PatternKind::Array { .. } => "TypeError: undefined is not iterable (cannot read property Symbol(Symbol.iterator))".to_string(),
            _ => return init,
        };
        self.record_present(init, &msg)
    }

    /// `h` as its `V` when it is a trusted read, else `h` as it is; a missing value stops the
    /// program with `msg`.
    pub(super) fn record_present(&mut self, h: hir::Expr, msg: &str) -> hir::Expr {
        let Some(v) = self.trusted_value(&h) else {
            return h;
        };
        let span = h.span;
        self.checked_present(h, msg, Some(v), true, span)
    }

    /// Does `r[k] op= v` / `r[k]++` on an open record with values `v` read `r[k]` as a `V`
    /// (`v` holds no nothing of its own and is not dynamic)?
    pub(super) fn open_compound_reads(&self, v: TyId) -> bool {
        self.cx.ty.nothing_of(v).is_none() && self.cx.dynamic(v).is_none()
    }

    /// `obj.prop` as the place an assignment writes (`field_access`): a missing record value
    /// there is Node's `Cannot set properties …` for `=`, and `Cannot read …` for an update,
    /// which reads first.
    pub(super) fn written_field(
        &mut self,
        obj: hir::Expr,
        prop: &ast::Ident,
        span: Span,
    ) -> Option<hir::Expr> {
        let outer = std::mem::replace(&mut self.f.write_target, true);
        let place = self.field_access(obj, prop, crate::body::Want::BorrowMut, span);
        self.f.write_target = outer;
        place
    }

    /// Marks the local `l`, declared with the annotation `ann` from `init`, as a trusted local
    /// when it has no annotation and `init` is a trusted read (`const entry = r[k]`).
    pub(crate) fn note_trusted_local(
        &mut self,
        l: hir::LocalId,
        ann: Option<TyId>,
        init: Option<&hir::Expr>,
    ) {
        let init = init.filter(|_| ann.is_none());
        if let Some(v) = init.and_then(|h| self.trusted_value(h)) {
            self.f.trusted_locals.insert(l, v);
        }
    }

    /// A read of local `l` at `span`: a trusted local's read is a record read.
    pub(super) fn note_trusted_read(&mut self, l: hir::LocalId, span: Span) {
        if let Some(&v) = self.f.trusted_locals.get(&l) {
            self.f.record_reads.insert(span, v);
        }
    }
}

fn str_expr(s: &str, span: Span) -> ast::Expr {
    ast::Expr {
        id: ast::NodeId(u32::MAX),
        kind: ast::ExprKind::Lit(ast::Lit::Str(s.into())),
        span,
    }
}

/// How Node names an expression in a `TypeError` (`… is not a function`): `f`, `o.f`, `o[k]`,
/// `o.f` for `o["f"]`, `f(...)` for a call, `(intermediate value)` for anything else.
fn node_text(e: &ast::Expr) -> String {
    node_text_as(e, true)
}

/// [`node_text`]; without `calls`, a call is named by its callee alone, as `… is not iterable`
/// names `r[k("a")]` `r[k]`.
fn node_text_as(e: &ast::Expr, calls: bool) -> String {
    use ast::ExprKind as A;
    let text = |e: &ast::Expr| node_text_as(e, calls);
    match &e.kind {
        A::Ident(id) => id.name.clone(),
        A::This => "this".to_string(),
        A::Paren(x) => text(x),
        A::Member { object, prop, .. } => format!("{}.{}", text(object), prop.name),
        A::Index { object, index, .. } => {
            let o = text(object);
            match &index.kind {
                A::Lit(ast::Lit::Str(s)) => format!("{o}.{s}"),
                A::Lit(ast::Lit::Int { value, .. }) => format!("{o}[{value}]"),
                _ => format!("{o}[{}]", text(index)),
            }
        }
        A::Call { callee, .. } if calls => format!("{}(...)", text(callee)),
        A::Call { callee, .. } => text(callee),
        _ => "(intermediate value)".to_string(),
    }
}
