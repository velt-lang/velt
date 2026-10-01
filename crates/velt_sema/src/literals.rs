//! Literal types (`"circle"`, `42`, `-1`, `1.5`, `true`; docs/reference/types.md "Literal types"):
//! `TyKind::Literal`, a zero-sized singleton type whose *base* is `string`, a number type or
//! `bool`. A literal type converts implicitly to its base (and a union of literals to the base
//! they share); literal expressions only take a literal type where one is expected
//! (`body::expr::literal_types`).

use velt_common::Span;
use velt_syntax::ast;

use crate::ctx::Ctx;
use crate::hir::{FloatTy, IntTy, LitValue, TyId, TyKind};
use crate::types::{int_max, int_name};

impl Ctx<'_> {
    pub fn lit_type(&mut self, v: LitValue) -> TyId {
        self.ty.intern(TyKind::Literal(v))
    }

    /// The value of literal type `t`.
    pub fn lit_value(&self, t: TyId) -> Option<LitValue> {
        match self.ty.kind(t) {
            TyKind::Literal(v) => Some(v.clone()),
            _ => None,
        }
    }

    /// The base type of a literal value (`string`, its number type, `bool`).
    pub fn lit_base(&mut self, v: &LitValue) -> TyId {
        match v {
            LitValue::Str(_) => self.ty.str_,
            LitValue::Bool(_) => self.ty.bool_,
            LitValue::Int(i, _) => self.ty.intern(TyKind::Int(*i)),
            LitValue::Float(f, _) => self.ty.intern(TyKind::Float(*f)),
        }
    }

    /// `t` with literal types replaced by their base: a literal → its base, a union whose
    /// members are all literals of one base (or that base) → the base; other types unchanged.
    pub fn widened(&mut self, t: TyId) -> TyId {
        if let Some(v) = self.lit_value(t) {
            return self.lit_base(&v);
        }
        let Some(members) = self.union_members(t) else {
            return t;
        };
        let mut base = None;
        for m in members {
            let b = match self.lit_value(m) {
                Some(v) => self.lit_base(&v),
                None => m,
            };
            if base.is_some_and(|x| x != b) {
                return t;
            }
            base = Some(b);
        }
        base.unwrap_or(t)
    }

    /// Does `t` (or a member of union `t`) involve a literal type?
    pub fn has_literal_member(&mut self, t: TyId) -> bool {
        if self.lit_value(t).is_some() {
            return true;
        }
        self.union_members(t)
            .is_some_and(|ms| ms.iter().any(|m| self.lit_value(*m).is_some()))
    }

    /// The literal type written as `l` (a type annotation); `None` after reporting an error.
    /// Integers are `i64` and floats `f64` unless suffixed (`1u8`).
    pub fn lit_value_of(&mut self, l: &ast::SignedLit, span: Span) -> Option<LitValue> {
        let suffix_ty = |cx: &mut Self, s: &Option<String>| match s {
            Some(s) => cx.ty.primitive(s),
            None => None,
        };
        match &l.lit {
            ast::Lit::Str(s) if !l.negative => Some(LitValue::Str(s.clone())),
            ast::Lit::Bool(b) if !l.negative => Some(LitValue::Bool(*b)),
            ast::Lit::Int { value, suffix } => {
                let t = suffix_ty(self, suffix).unwrap_or(self.ty.i64);
                match self.ty.kind(t).clone() {
                    TyKind::Int(it) => self.int_lit_value(it, *value, l.negative, span),
                    TyKind::Float(ft) => Some(float_value(ft, *value as f64, l.negative)),
                    _ => {
                        self.err("invalid suffix for a number literal type", span);
                        None
                    }
                }
            }
            ast::Lit::Float { value, suffix } => {
                let t = suffix_ty(self, suffix).unwrap_or(self.ty.f64);
                match self.ty.kind(t).clone() {
                    TyKind::Float(ft) => Some(float_value(ft, *value, l.negative)),
                    _ => {
                        self.err("invalid suffix for a float literal type", span);
                        None
                    }
                }
            }
            _ => {
                self.err("this literal cannot be used as a type", span);
                None
            }
        }
    }

    fn int_lit_value(
        &mut self,
        it: IntTy,
        value: u128,
        negative: bool,
        span: Span,
    ) -> Option<LitValue> {
        if value > int_max(it, negative) || (negative && !it.is_signed() && value != 0) {
            self.err(format!("literal out of range for `{}`", int_name(it)), span);
            return None;
        }
        let v = value as i128;
        Some(LitValue::Int(it, if negative { -v } else { v }))
    }
}

/// Does the literal expression `l` denote the value `v` (a suffix must name `v`'s type)?
pub(crate) fn lit_matches(v: &LitValue, l: &ast::SignedLit) -> bool {
    let suffix_ok = |s: &Option<String>, name: &str| s.as_deref().is_none_or(|s| s == name);
    let signed = |n: u128| {
        let n = n as i128;
        if l.negative {
            -n
        } else {
            n
        }
    };
    let float = |f: f64| if l.negative { -f } else { f };
    let float_name = |ft: &FloatTy| if *ft == FloatTy::F32 { "f32" } else { "f64" };
    match (v, &l.lit) {
        (LitValue::Str(a), ast::Lit::Str(b)) => !l.negative && a == b,
        (LitValue::Bool(a), ast::Lit::Bool(b)) => !l.negative && a == b,
        (LitValue::Int(it, n), ast::Lit::Int { value, suffix }) => {
            suffix_ok(suffix, int_name(*it)) && *value <= i128::MAX as u128 && signed(*value) == *n
        }
        (LitValue::Float(ft, bits), ast::Lit::Float { value, suffix }) => {
            suffix_ok(suffix, float_name(ft)) && float(*value) == f64::from_bits(*bits)
        }
        (LitValue::Float(ft, bits), ast::Lit::Int { value, suffix }) => {
            suffix_ok(suffix, float_name(ft)) && float(*value as f64) == f64::from_bits(*bits)
        }
        _ => false,
    }
}

fn float_value(ft: FloatTy, v: f64, negative: bool) -> LitValue {
    LitValue::Float(ft, (if negative { -v } else { v }).to_bits())
}

/// How a literal type is written (`"circle"`, `42`, `5i32`, `1.5`, `true`).
pub(crate) fn display_lit(v: &LitValue) -> String {
    match v {
        LitValue::Str(s) => format!("{s:?}"),
        LitValue::Bool(b) => b.to_string(),
        LitValue::Int(IntTy::I64, n) => n.to_string(),
        LitValue::Int(it, n) => format!("{n}{}", int_name(*it)),
        LitValue::Float(ft, bits) => {
            let f = f64::from_bits(*bits);
            let shown = if f.fract() == 0.0 && f.is_finite() {
                format!("{f:.1}")
            } else {
                f.to_string()
            };
            match ft {
                FloatTy::F64 => shown,
                FloatTy::F32 => format!("{shown}f32"),
            }
        }
    }
}
