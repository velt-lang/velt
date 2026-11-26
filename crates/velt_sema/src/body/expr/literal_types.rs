//! Literal types in expressions (https://veltlang.com/docs/reference/types/ "Literal types"). A literal (`"circle"`, `-1`,
//! `true`) checked where a literal type is expected — the literal type itself or a union with
//! literal members — takes that literal type; its HIR value is `Lit(Unit)` (literal types are
//! zero-sized). Everywhere else literals keep their base types (TS widening, simplified).
//! Literal-typed values convert to their base type where the base is expected, and are widened
//! for operators (`widen_value`).

use velt_common::Span;
use velt_syntax::ast;

use crate::body::narrow::literal_of;
use crate::body::places::is_place;
use crate::body::FnCx;
use crate::hir::{self, ExprKind as H, LitValue, TyId, TyKind};
use crate::literals::lit_matches;

impl FnCx<'_, '_> {
    /// `e` as a literal-typed value if it is a literal and `exp` involves literal types.
    pub(super) fn literal_typed(&mut self, e: &ast::Expr, exp: Option<TyId>) -> Option<hir::Expr> {
        let exp = self.hint(exp)?;
        if !self.cx.has_literal_member(exp) {
            return None;
        }
        let lit = literal_of(e)?;
        if let Some(v) = self.cx.lit_value(exp) {
            if lit_matches(&v, &lit) {
                return Some(self.lit_const(exp, e.span));
            }
            // A different literal: typed as its own literal type, so the mismatch names it.
            return self.own_literal(&lit, Some(&v), e.span);
        }
        let members = self.cx.union_members(exp)?;
        let exact = members
            .iter()
            .copied()
            .find(|m| self.cx.lit_value(*m).is_some_and(|v| lit_matches(&v, &lit)));
        if let Some(m) = exact {
            return Some(self.lit_const(m, e.span));
        }
        if self.lit_member(exp, &lit).is_ok() {
            // A member of the literal's base type (`string` in `"a" | string`) takes it.
            return None;
        }
        let like = members.iter().find_map(|m| self.cx.lit_value(*m));
        self.own_literal(&lit, like.as_ref(), e.span)
    }

    /// The (zero-sized) value of literal type `ty`.
    pub(crate) fn lit_const(&self, ty: TyId, span: Span) -> hir::Expr {
        self.mk(H::Lit(hir::Lit::Unit), ty, span)
    }

    /// `lit` typed as the literal type it denotes (numbers take the type of `like` when it is a
    /// literal of the same kind, else `i64` / `f64`).
    fn own_literal(
        &mut self,
        lit: &ast::SignedLit,
        like: Option<&LitValue>,
        span: Span,
    ) -> Option<hir::Expr> {
        let lit = match (&lit.lit, like) {
            (
                ast::Lit::Int {
                    value,
                    suffix: None,
                },
                // Not `i64`, an unsuffixed literal's own type (from 2^53 on, an `f64`).
                Some(LitValue::Int(it, _)),
            ) if *it != crate::hir::IntTy::I64 => {
                let name = crate::types::int_name(*it);
                ast::SignedLit {
                    lit: ast::Lit::Int {
                        value: *value,
                        suffix: Some(name.to_string()),
                    },
                    negative: lit.negative,
                }
            }
            _ => lit.clone(),
        };
        let v = self.cx.lit_value_of(&lit, span)?;
        let t = self.cx.lit_type(v);
        Some(self.lit_const(t, span))
    }

    /// The base-typed value of a literal (`"circle"`, `-1`, `1.5`, `true`).
    pub(crate) fn base_lit_expr(&mut self, v: &LitValue, span: Span) -> hir::Expr {
        let ty = self.cx.lit_base(v);
        let (lit, negative) = match v {
            LitValue::Str(s) => (hir::Lit::Str(s.clone()), false),
            LitValue::Bool(b) => (hir::Lit::Bool(*b), false),
            LitValue::Int(_, n) => (hir::Lit::Int(n.unsigned_abs()), *n < 0),
            LitValue::Float(_, bits) => {
                let f = f64::from_bits(*bits);
                (hir::Lit::Float(f.abs()), f.is_sign_negative())
            }
        };
        let e = self.mk(H::Lit(lit), ty, span);
        if !negative {
            return e;
        }
        let kind = H::Unary {
            op: hir::UnOp::Neg,
            expr: Box::new(e),
        };
        self.mk(kind, ty, span)
    }

    /// A literal-typed value `h` as its base-typed constant (`h` is still evaluated when it
    /// is not a plain place).
    pub(super) fn lit_to_base(&mut self, h: hir::Expr, v: &LitValue) -> hir::Expr {
        let span = h.span;
        let c = self.base_lit_expr(v, span);
        if is_place(&h) || matches!(h.kind, H::Lit(_)) {
            return c;
        }
        let ty = c.ty;
        let block = hir::Block {
            stmts: vec![hir::Stmt {
                kind: hir::StmtKind::Expr(h),
                span,
            }],
            value: Some(Box::new(c)),
            span,
        };
        self.mk(H::Block(block), ty, span)
    }

    /// `h` with literal types widened to their base (`"a"` → `string`, `"a" | "b"` → `string`),
    /// for operands of operators and other untyped consumers.
    pub(crate) fn widen_value(&mut self, h: hir::Expr) -> hir::Expr {
        let w = self.cx.widened(h.ty);
        if w == h.ty {
            return h;
        }
        self.try_coerce(h, w).unwrap_or_else(|h| h)
    }

    /// The operands of `op` with literal types taking part as their base type (`c.kind + "!"`,
    /// `q + q` of `q: "a" | "b"`); `===` compares a union of literals as it is. A number literal
    /// type (`a: 1 | 2`, an `i64` underneath) is a JS number: arithmetic on one computes on
    /// `number`s, so `a / 4` is `0.25` and `(a + b) / 4` is `0.75`, as in JS.
    pub(crate) fn literal_operand_bases(
        &mut self,
        op: ast::BinaryOp,
        l: hir::Expr,
        r: hir::Expr,
    ) -> (hir::Expr, hir::Expr) {
        use ast::BinaryOp as B;
        let eq = matches!(op, B::Eq | B::NotEq | B::StrictEq | B::StrictNotEq);
        // A union of integer and float literals (`0 | 1 | 2.5`) is a `number` to an operator,
        // as it is in TypeScript; `===` compares it as it is (#3815).
        let (l, r) = match eq {
            true => (l, r),
            false => (self.mixed_as_number(l), self.mixed_as_number(r)),
        };
        let lits = self.cx.lit_value(l.ty).is_some() || !eq && self.cx.widened(l.ty) != l.ty;
        if l.ty == r.ty && !lits {
            return (l, r);
        }
        let arith = matches!(op, B::Add | B::Sub | B::Mul | B::Div | B::Rem | B::Pow);
        let (ll, rl) = (self.number_literal(l.ty), self.number_literal(r.ty));
        // A bitwise operator beside a `number` takes the literal as the `number` it is too
        // (`y | n` with `y: number`, `n: 0 | 1`; also `c.v |= n` through an accessor, #3747).
        let f64 = self.cx.ty.f64;
        let bitwise = super::int32::is_bitwise(op) && ((ll && r.ty == f64) || (rl && l.ty == f64));
        let float = (arith && (ll || rl)) || bitwise;
        let (l, r) = (self.widen_value(l), self.widen_value(r));
        if !float {
            return (l, r);
        }
        let float = |s: &mut Self, h: hir::Expr| match h.ty == s.cx.ty.i64 {
            true => s.int_to_float(h, f64),
            false => h,
        };
        (float(self, l), float(self, r))
    }

    /// `h`, of an integer literal type or a union of them (`5`, `0 | 1`: an `i64` underneath),
    /// as the `number` it is in TypeScript, converted to `exp` from there: `number` itself, a
    /// union holding `number` (`string | number`), or a `number | null`. Tried after the other
    /// conversions, so a union that holds the literals themselves keeps them. `Err(h)` when `h`
    /// is no such type or the `number` converts to `exp` no better. One literal is the `number`
    /// constant [`int_literal_as_number`](Self::int_literal_as_number) makes of it.
    pub(super) fn int_literal_to(
        &mut self,
        h: hir::Expr,
        exp: TyId,
    ) -> Result<hir::Expr, hir::Expr> {
        let f64 = self.cx.ty.f64;
        let mixed = self.mixed_number_literal(h.ty);
        if exp == h.ty || !(mixed || self.number_literal(h.ty)) || !self.holds_number(exp, 4) {
            return Err(h);
        }
        let original = h.clone();
        let number = match self.cx.lit_value(h.ty) {
            _ if mixed => self.mixed_as_number(h),
            Some(hir::LitValue::Int(..)) => self.int_literal_as_number(h),
            _ => {
                let int = self.widen_value(h);
                if int.ty != self.cx.ty.i64 {
                    return Err(original);
                }
                self.int_to_float(int, f64)
            }
        };
        if number.ty != f64 {
            return Err(original);
        }
        match exp == f64 {
            true => Ok(number),
            false => self.try_coerce(number, exp).map_err(|_| original),
        }
    }

    /// `v`, the operand of a compound assignment to a place of type `place`: a value of an
    /// integer literal type or a union of them as the `number` it is when the place is a `number`
    /// (`x += n`, `x |= n` with `n: 0 | 1`), else `v` as it is. Taken before the bitwise path,
    /// which takes `number`s (#3747).
    pub(super) fn compound_literal_operand(&mut self, place: TyId, v: hir::Expr) -> hir::Expr {
        if place != self.cx.ty.f64 {
            return v;
        }
        self.int_literal_to(v, place).unwrap_or_else(|v| v)
    }

    /// Can a `number` convert to `t`: `number` itself, or a union or an option holding one
    /// (`depth` bounds the nesting looked into)?
    fn holds_number(&mut self, t: TyId, depth: u32) -> bool {
        if t == self.cx.ty.f64 {
            return true;
        }
        if depth == 0 {
            return false;
        }
        if let Some(inner) = self.cx.ty.opt_payload(t) {
            return self.holds_number(inner, depth - 1);
        }
        match self.cx.union_members(t) {
            Some(members) => members.into_iter().any(|m| self.holds_number(m, depth - 1)),
            None => false,
        }
    }

    /// The operand of a unary `-`, `+` or `~`: a value of a number literal type or a union of
    /// them as its base, which the operators take. An integer one (`n: 0 | 1`, an `i64`
    /// underneath) is the `number` it is in TypeScript, so `-n` of 0 is `-0` and `~n` is
    /// JavaScript's 32-bit not (#3784). Any other value as it is.
    pub(super) fn unary_literal_operand(&mut self, h: hir::Expr) -> hir::Expr {
        if self.mixed_number_literal(h.ty) {
            return self.mixed_as_number(h);
        }
        let w = self.cx.widened(h.ty);
        if w == h.ty || !self.cx.ty.is_numeric(w) {
            return h;
        }
        let base = self.widen_value(h);
        match base.ty == self.cx.ty.i64 {
            true => self.int_to_float(base, self.cx.ty.f64),
            false => base,
        }
    }

    /// Is `t` a union of unsuffixed integer and float literals, both (`0 | 1 | 2.5`)? Its
    /// members have two base types, `i64` and `f64`, so [`Ctx::widened`] leaves it as it is,
    /// though in TypeScript each member, and so the union, is a `number` (#3815).
    pub(super) fn mixed_number_literal(&mut self, t: TyId) -> bool {
        let Some(members) = self.cx.union_members(t) else {
            return false;
        };
        let (mut int, mut float) = (false, false);
        for m in members {
            match self.cx.lit_value(m) {
                Some(hir::LitValue::Int(hir::IntTy::I64, _)) => int = true,
                Some(hir::LitValue::Float(hir::FloatTy::F64, _)) => float = true,
                _ => return false,
            }
        }
        int && float
    }

    /// `h` as the `number` it is when it is of a union of integer and float literals
    /// ([`mixed_number_literal`](Self::mixed_number_literal)): each member converted to `number`
    /// (an integer through [`int_literal_to`](Self::int_literal_to)). Any other value as it is.
    pub(super) fn mixed_as_number(&mut self, h: hir::Expr) -> hir::Expr {
        if !self.mixed_number_literal(h.ty) {
            return h;
        }
        let f64 = self.cx.ty.f64;
        self.union_to_common(h, f64).unwrap_or_else(|h| h)
    }

    /// Is `t` a number literal type or a union of them, of `i64`s (`1 | 2`)?
    fn number_literal(&mut self, t: TyId) -> bool {
        let w = self.cx.widened(t);
        w != t && w == self.cx.ty.i64
    }

    /// `h` of a string literal type or a union of them (`"x" | "y"`) as the `string` it is, for
    /// what reads a string's characters: `for...of`, `s[0]`, `[...s]`.
    pub(crate) fn widen_str_literals(&mut self, h: hir::Expr) -> hir::Expr {
        match h.ty != self.cx.ty.str_ && self.cx.widened(h.ty) == self.cx.ty.str_ {
            true => self.widen_value(h),
            false => h,
        }
    }

    /// Members of a literal type or a union of literals are its base type's (`l.length`,
    /// `kind.toUpperCase()` on `"lo" | "mid"`), unless the union itself has the member (an
    /// `extend` block on it).
    pub(crate) fn widen_literal_receiver(&mut self, h: hir::Expr, name: &str) -> hir::Expr {
        if !self.cx.has_literal_member(h.ty) || self.cx.widened(h.ty) == h.ty {
            return h;
        }
        if self.cx.lit_value(h.ty).is_none() && self.method_exists(h.ty, name) {
            return h;
        }
        self.widen_value(h)
    }

    /// Is `t` a string enum (`enum Dir { Up = "UP" }`)?
    pub(crate) fn is_string_enum(&self, t: TyId) -> bool {
        match self.cx.ty.kind(t) {
            TyKind::Adt(d, _) => self
                .cx
                .enum_info(*d)
                .is_some_and(|e| !e.is_union && e.variants.iter().any(|v| v.str_value.is_some())),
            _ => false,
        }
    }

    /// A string enum value as its string (`match (d) { Dir.Up => "UP", ... }`).
    pub(super) fn string_enum_to_str(&mut self, h: hir::Expr) -> hir::Expr {
        let TyKind::Adt(def, _) = self.cx.ty.kind(h.ty).clone() else {
            return h;
        };
        let values: Vec<String> = self
            .cx
            .enum_info(def)
            .map(|e| {
                e.variants
                    .iter()
                    .map(|v| v.str_value.clone().unwrap_or_default())
                    .collect()
            })
            .unwrap_or_default();
        let (span, sty) = (h.span, h.ty);
        let arms = values
            .iter()
            .enumerate()
            .map(|(i, s)| hir::Arm {
                pat: self.pat(
                    hir::PatKind::Variant {
                        def,
                        variant: i as u32,
                        args: vec![],
                    },
                    sty,
                    span,
                ),
                guard: None,
                body: self.str_lit(s, span),
            })
            .collect();
        let kind = H::Match {
            scrutinee: Box::new(h),
            arms,
        };
        self.mk(kind, self.cx.ty.str_, span)
    }
}
