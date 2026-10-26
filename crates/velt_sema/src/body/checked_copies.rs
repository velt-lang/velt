//! Where a narrowed value that a call may have changed reads as its payload, and untyped copies
//! of one (#1280).
//!
//! A module variable (`global_narrow`) or a field path (`field_narrow`, also `r[k]`) that a
//! null check narrowed may be set to `null` by any call before a read, which TypeScript ignores.
//! Such a read is the payload, checked where it happens, only where the payload is needed: as a
//! receiver (`x.m()`, `x.f = v`), a callee, an operand of arithmetic or a comparison
//! (`n + 1`, `n > 2`; `+` with a string converts it as it is), a spread (`...xs`), a value of a
//! type that does not take the value as it is (`use(x)` with `a: Foo`, `return x`), or what a
//! write goes through. Everywhere else (`console.log(x)`, a template, `x === y`, `"a" + x`, a
//! `Foo | null | undefined` or `Base | null` parameter, an untyped object or array element) the
//! value is read as it is, so a program that reads it after a call cleared it sees `null`, as
//! in JavaScript.
//!
//! An untyped `let c = x` / `const c = x` of such a value is a *checked copy*: `c` has the
//! nullable type and the narrowing of `x`, and its reads follow the same rule, checked where the
//! payload is needed (`c.n` stops with a `TypeError` where JavaScript throws one, `console.log(c)`
//! prints `null`). A copy of a field `typeof` narrowed to one member has the union type,
//! narrowed to that member, and its member reads are checked (`checked_member_read`). A closure
//! that captures a copy checks its reads too, and a copy of a copy is one too.

use velt_common::Span;
use velt_syntax::ast;

use super::narrow::Fact;
use super::{FnCx, Want};
use crate::hir::{self, ExprKind as H, LocalId, TyId};

impl FnCx<'_, '_> {
    /// Does the read at `span` (expected type `exp`) need the payload of a narrowed value of
    /// type `raw` (where a `raw` is expected, the value as it is will do)?
    pub(crate) fn needs_payload(
        &mut self,
        span: Span,
        exp: Option<TyId>,
        want: Want,
        taken: bool,
        raw: TyId,
    ) -> bool {
        if self.raw_read == Some(span) || self.as_is_spans.contains(&span) {
            return false;
        }
        if want == Want::BorrowMut || self.payload_spans.contains(&span) {
            return true;
        }
        // With `taken` (a field path of a local, which was always its payload before), a value
        // taken where no type is expected (an untyped array or object element) is the payload,
        // as TypeScript types it; an untyped `let c = x` is a checked copy instead.
        if taken && want == Want::Move && exp.is_none() {
            return true;
        }
        exp.is_some_and(|t| {
            self.cx.dynamic(t).is_none()
                && !matches!(self.cx.ty.kind(t), hir::TyKind::Param(_))
                && !self.fits_as_is(raw, t)
        })
    }

    /// Does a value of type `raw` fit where a `t` is expected as it is: `t` has the nothings of
    /// `raw` (`Box | null` where a `Box | null | undefined` or a `Base | null` is expected) and
    /// its value's members, when it is a union (`string | number` where a `string | number |
    /// null` is)?
    fn fits_as_is(&mut self, raw: TyId, t: TyId) -> bool {
        if raw == t {
            return true;
        }
        let covered = match (self.cx.ty.nothing_of(raw), self.cx.ty.nothing_of(t)) {
            (None, _) => true,
            (Some(_), None) => false,
            (Some(r), Some(n)) => n == r || n == hir::Nothing::Both,
        };
        let rc = self.cx.ty.core(raw).unwrap_or(raw);
        let tc = self.cx.ty.core(t).unwrap_or(t);
        let tm = self.cx.union_members(tc).unwrap_or_else(|| vec![tc]);
        let rm = self.cx.union_members(rc).unwrap_or_else(|| vec![rc]);
        // A nullable value of one type converts as it is (`Box | null` as a `Base | null`).
        let one = self.cx.ty.nothing_of(raw).is_some() && self.cx.union_members(rc).is_none();
        covered && (one || rm.iter().all(|m| tm.contains(m)))
    }

    /// Is `e` a string for `+` (a string literal or template, a concatenation with one, or an
    /// expression whose value is a `string`): `+` with one converts the other operand as it
    /// is, `null` included.
    pub(crate) fn string_operand(&mut self, e: &ast::Expr) -> bool {
        let mut inner = e;
        while let ast::ExprKind::Paren(x) = &inner.kind {
            inner = x;
        }
        match &inner.kind {
            ast::ExprKind::Lit(ast::Lit::Str(_)) | ast::ExprKind::Template { .. } => true,
            ast::ExprKind::Lit(_) => false,
            ast::ExprKind::Binary {
                op: ast::BinaryOp::Add,
                lhs,
                rhs,
            } if self.string_operand(lhs) || self.string_operand(rhs) => true,
            _ => self.peek_ty(inner).is_some_and(|t| {
                let p = self.cx.ty.core(t).unwrap_or(t);
                p == self.cx.ty.str_
            }),
        }
    }

    /// Is argument `e`, of parameter type `pty`, of a type parameter this call infers from it
    /// (no type argument, receiver or context fixed it)? It is passed as it is when the call's
    /// result `ret` does not mention the parameter (`JSON.stringify(x)`), or when it is a
    /// module variable or a field path of one, whose type main infers as it is too (`id(x)` is a
    /// `Box | null`); a field path of a local stays its payload (`Promise.reject(this.err)`).
    fn inferred_from_arg(
        &mut self,
        e: &ast::Expr,
        (pty, ret): (TyId, TyId),
        (given, context): (&[bool], &[Option<TyId>]),
        expected: TyId,
    ) -> bool {
        match self.cx.ty.kind(pty).clone() {
            hir::TyKind::Param(k) => {
                let mut in_ret = vec![];
                crate::types::collect_params(&self.cx.ty, ret, &mut in_ret);
                let open = !in_ret.contains(&k) || self.as_is_source(e);
                let k = k as usize;
                open && !given.get(k).copied().unwrap_or(true)
                    && context.get(k).is_none_or(Option::is_none)
            }
            _ => matches!(self.cx.ty.kind(expected), hir::TyKind::Param(_)),
        }
    }

    /// Argument `e` of a parameter of type `pty` (of a call returning `ret`), checked against
    /// `expected` (`inferred_from_arg`).
    pub(crate) fn param_arg(
        &mut self,
        e: &ast::Expr,
        sig: (TyId, TyId),
        slots: (&[bool], &[Option<TyId>]),
        (expected, want): (TyId, Want),
    ) -> hir::Expr {
        match self.inferred_from_arg(e, sig, slots, expected) {
            true => self.generic_arg(e, expected, want),
            false => self.expr(e, Some(expected), want),
        }
    }

    /// Argument `e` of a type parameter the call infers from it, as it is; a maybe-`undefined`
    /// value as its payload, as before (`JSON.stringify` of one is not supported).
    fn generic_arg(&mut self, e: &ast::Expr, expected: TyId, want: Want) -> hir::Expr {
        let h = self.as_is_expr(e, Some(expected), want);
        match self.cx.ty.nothing_of(h.ty) {
            Some(n) if n.has_undefined() => self.expr(e, Some(expected), want),
            _ => h,
        }
    }

    /// Does this function (or one it is in) hold a narrowing a call may break (`needs_payload`
    /// matters only then)?
    pub(crate) fn has_breakable_narrowing(&self) -> bool {
        !self.f.field_tokens.is_empty()
            || !self.f.global_tokens.is_empty()
            || std::iter::once(&self.f)
                .chain(self.outer.iter())
                .any(|f| !f.checked_copies.is_empty())
    }

    /// `e` checked where its value is used as it is (`s += x` on a string): a narrowed name or
    /// path is not taken as its payload.
    pub(crate) fn as_is_expr(&mut self, e: &ast::Expr, exp: Option<TyId>, want: Want) -> hir::Expr {
        let mut inner = e;
        while let ast::ExprKind::Paren(x) = &inner.kind {
            inner = x;
        }
        let outer = self.raw_read.replace(inner.span);
        let h = self.expr(e, exp, want);
        self.raw_read = outer;
        h
    }

    /// `f` with `e`'s value taken as a payload (`needs_payload`).
    pub(crate) fn with_payload<R>(&mut self, e: &ast::Expr, f: impl FnOnce(&mut Self) -> R) -> R {
        let mut inner = e;
        while let ast::ExprKind::Paren(x) = &inner.kind {
            inner = x;
        }
        self.with_payload_at(inner.span, f)
    }

    /// `f` with the read at `span` taken as a payload (a callee name).
    pub(crate) fn with_payload_at<R>(&mut self, span: Span, f: impl FnOnce(&mut Self) -> R) -> R {
        self.payload_spans.push(span);
        let r = f(self);
        self.payload_spans.pop();
        r
    }

    /// `e` checked as an operand whose payload is used (`n + 1`, `...xs`).
    pub(crate) fn payload_expr(
        &mut self,
        e: &ast::Expr,
        exp: Option<TyId>,
        want: Want,
    ) -> hir::Expr {
        self.with_payload(e, |s| s.expr(e, exp, want))
    }

    /// `object` evaluated as the receiver of a member access or call.
    pub(crate) fn receiver_expr(&mut self, object: &ast::Expr) -> hir::Expr {
        self.payload_expr(object, None, Want::Borrow)
    }

    /// The initializer `e` of an untyped `let` or `const`: a narrowed module variable or field
    /// path is read as it is (the local is a checked copy, `note_checked_copy`).
    pub(crate) fn copy_init(&mut self, e: &ast::Expr, want: Want) -> hir::Expr {
        let mut inner = e;
        while let ast::ExprKind::Paren(x) = &inner.kind {
            inner = x;
        }
        let path = matches!(
            inner.kind,
            ast::ExprKind::Ident(_)
                | ast::ExprKind::Member {
                    optional: false,
                    ..
                }
                | ast::ExprKind::Index {
                    optional: false,
                    ..
                }
        );
        if !path || self.narrowed_source(inner).is_none() {
            return self.expr(e, None, want);
        }
        // A function stays its payload (`const f = o.cb; f()`), as before.
        let callable = self
            .peek_ty(inner)
            .and_then(|t| self.cx.ty.opt_payload(t))
            .is_some_and(|p| {
                matches!(
                    self.cx.ty.kind(p),
                    hir::TyKind::Closure(_) | hir::TyKind::FnPtr { .. }
                )
            });
        if callable {
            return self.payload_expr(e, None, want);
        }
        let outer = self.raw_read.replace(inner.span);
        let h = self.expr(e, None, want);
        self.raw_read = outer;
        h
    }

    /// The untyped local `local`, just declared from `init`: a checked copy when `init` is a
    /// narrowed module variable or field path read as it is.
    pub(crate) fn note_checked_copy(
        &mut self,
        local: LocalId,
        untyped: bool,
        init: Option<&ast::Expr>,
    ) {
        let Some(init) = init.filter(|_| untyped) else {
            return;
        };
        let mut inner = init;
        while let ast::ExprKind::Paren(x) = &inner.kind {
            inner = x;
        }
        // `const c = st.v` after `typeof st.v === "string"`: `c` holds the union, narrowed to
        // that member, whose reads are checked (`UnwrapVariant`).
        let nullable = self.cx.ty.opt_payload(self.local_ty(local)).is_some();
        if let Some(vs) = self.member_source(inner).filter(|_| !nullable) {
            self.f.checked_copies.insert(local);
            self.narrow(&Fact::Members(local, vs));
            return;
        }
        if !nullable {
            return;
        }
        let token = self.narrowed_source(inner);
        if token.is_some() {
            self.f.checked_copies.insert(local);
            self.narrow(&Fact::NonNull(local));
        }
    }

    /// The token of `inner` (a name or a field path) when a null test narrowed it and a call
    /// may have changed it since: a module variable, a variable a closure assigns, a field path.
    fn narrowed_source(&mut self, inner: &ast::Expr) -> Option<LocalId> {
        let token = match &inner.kind {
            // A variable a closure assigns is checked the same way (`closure_assigned`).
            ast::ExprKind::Ident(_) => match self.named_local(inner) {
                Some(l) if self.closure_assignment(l).is_some() || self.is_checked_copy(l) => {
                    Some(l)
                }
                Some(_) => None,
                None => self.existing_global_token(inner),
            },
            ast::ExprKind::Member {
                optional: false, ..
            }
            | ast::ExprKind::Index {
                optional: false, ..
            } => self.existing_field_token(inner),
            _ => None,
        };
        token.filter(|t| self.is_narrowed(*t))
    }

    /// The member a `typeof` test narrowed `inner` to, when it is a field path of a module
    /// variable or an `r[k]` (a field path of a local is read as its payload, as before).
    fn member_source(&mut self, inner: &ast::Expr) -> Option<Vec<u32>> {
        // A copy of a copy (`const d = c`).
        if let ast::ExprKind::Ident(_) = &inner.kind {
            let l = self
                .named_local(inner)
                .filter(|l| self.is_checked_copy(*l))?;
            return self.allowed_members(l).filter(|vs| vs.len() == 1);
        }
        if !matches!(
            inner.kind,
            ast::ExprKind::Member {
                optional: false,
                ..
            } | ast::ExprKind::Index {
                optional: false,
                ..
            }
        ) {
            return None;
        }
        let token = self.existing_field_token(inner)?;
        let (root, path) = self.token_path(token)?;
        let keyed = path.iter().any(|p| p.starts_with('\0'));
        let module = keyed || self.token_global_ty(root).is_some();
        let vs = self.allowed_members(token)?;
        (module && vs.len() == 1).then_some(vs)
    }

    /// May `e` read a narrowed value that a call cleared (a module variable, a field path, an
    /// `r[k]`, a checked copy or a variable a closure assigns, narrowed here)?
    pub(crate) fn may_read_cleared(&mut self, e: &ast::Expr) -> bool {
        let mut inner = e;
        while let ast::ExprKind::Paren(x) = &inner.kind {
            inner = x;
        }
        if let ast::ExprKind::Ident(_) = &inner.kind {
            if let Some(l) = self.named_local(inner) {
                if self.is_checked_copy(l) {
                    return true;
                }
            }
        }
        self.narrowed_source(inner).is_some() || self.member_source(inner).is_some()
    }

    /// Is `e` a narrowed value read as it is where no type is expected (a module variable, a
    /// field path of one, an `r[k]`, a checked copy, a variable a closure assigns; not a field
    /// path of a local, which an untyped element reads as its payload)?
    pub(crate) fn as_is_source(&mut self, e: &ast::Expr) -> bool {
        let mut inner = e;
        while let ast::ExprKind::Paren(x) = &inner.kind {
            inner = x;
        }
        if !self.may_read_cleared(inner) {
            return false;
        }
        let Some(token) = self.existing_field_token(inner) else {
            return true;
        };
        match self.token_path(token) {
            Some((root, path)) => {
                path.iter().any(|p| p.starts_with('\0')) || self.token_global_ty(root).is_some()
            }
            None => true,
        }
    }

    /// Is local `l` (of this frame, or the variable it captures) a checked copy?
    pub(crate) fn is_checked_copy(&self, l: LocalId) -> bool {
        let mut l = l;
        for f in std::iter::once(&self.f).chain(self.outer.iter().rev()) {
            if f.checked_copies.contains(&l) {
                return true;
            }
            match f.captures.iter().find(|c| c.inner == l) {
                Some(c) => l = c.outer,
                None => return false,
            }
        }
        false
    }

    /// The member `m` (variant `variant`) of checked copy `l` that a `typeof` test narrowed it
    /// to, checked: `match (l) { M(v) => v, _ => panic }` (a call may have changed the field
    /// it was copied from before the copy).
    pub(crate) fn checked_member_read(
        &mut self,
        l: LocalId,
        variant: u32,
        m: TyId,
        span: Span,
    ) -> hir::Expr {
        use hir::PatKind as P;
        let ty = self.local_ty(l);
        let def = self
            .cx
            .union_def(ty)
            .map(|(d, _)| d)
            .expect("ICE: a checked copy narrowed to a member is a union");
        let scrutinee = self.mk(H::Local(l, hir::UseMode::Borrow), ty, span);
        let (sub, value) = if self.cx.lit_value(m).is_some() {
            (self.pat(P::Wildcard, m, span), self.lit_const(m, span))
        } else {
            let mode = match self.cx.is_copy(m) {
                true => hir::UseMode::Copy,
                false => hir::UseMode::Borrow,
            };
            let b = self.new_local("<checked member>", m, false, span, super::LocalKind::Bind);
            let value = self.mk(H::Local(b, mode), m, span);
            (self.pat(P::Binding(b, mode), m, span), value)
        };
        let name = self.f.locals[l.0 as usize].name.clone();
        let msg = format!(
            "TypeError: `{name}` was copied from a checked value, but it is a value of another type"
        );
        let msg = self.str_lit(&msg, span);
        let never = self.cx.ty.never;
        let fail = self.intrinsic(hir::Intrinsic::Panic, vec![msg], never, span);
        let args = vec![sub];
        let arms = vec![
            hir::Arm {
                pat: self.pat(P::Variant { def, variant, args }, ty, span),
                guard: None,
                body: value,
            },
            hir::Arm {
                pat: self.pat(P::Wildcard, ty, span),
                guard: None,
                body: fail,
            },
        ];
        let kind = H::Match {
            scrutinee: Box::new(scrutinee),
            arms,
        };
        self.mk(kind, m, span)
    }

    /// A read of checked copy `l` that does not need its payload: the value as it is.
    pub(crate) fn checked_copy_read(
        &mut self,
        l: LocalId,
        exp: Option<TyId>,
        want: Want,
        span: Span,
    ) -> Option<hir::Expr> {
        if !self.is_checked_copy(l)
            || !(self.is_narrowed(l) || self.allowed_members(l).is_some())
            || self.needs_payload(span, exp, want, false, self.local_ty(l))
        {
            return None;
        }
        let ty = self.local_ty(l);
        let mode = self.use_mode(ty, want);
        Some(self.mk(H::Local(l, mode), ty, span))
    }
}
