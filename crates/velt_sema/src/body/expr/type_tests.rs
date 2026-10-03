//! Type tests: `typeof x` (as a string, and compared with a tag: `typeof x === "string"`),
//! `x instanceof C`, and `x == literal` on a union value. On unions (and `U | null`) each test is
//! a `match` on the variant (and `null`) yielding a bool; on other types `typeof` is a constant.
//! The flow narrowing the same conditions imply is computed in `body::narrow`.
//!
//! `typeof` follows JS: every number type is `"number"`, `string`, `boolean`, closures
//! `"function"`, everything else (classes, structs, arrays, maps, `null`) `"object"`.
//! `instanceof` compares classes (type arguments are not written): a member matches when its
//! class is `C` or a subclass of `C`; testing for a subclass of a member's class would need a
//! runtime downcast and is rejected.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::body::places::is_place;
use crate::body::{FnCx, Want};
use crate::ctx::{Ctx, Item};
use crate::hir::{self, DefId, ExprKind as H, PatKind as P, TyId};
use crate::unions::TYPEOF_TAGS;

/// Result of checking `a == b` where one side is a literal.
pub(crate) enum LitEq {
    /// A union compared with a literal of one of its members: the finished test.
    Done(hir::Expr),
    /// Ordinary operands (checked, in source order).
    Operands(hir::Expr, hir::Expr),
}

impl FnCx<'_, '_> {
    /// `typeof e` as a string value.
    pub(crate) fn typeof_value(&mut self, operand: &ast::Expr, span: Span) -> hir::Expr {
        let s = self.expr(operand, None, Want::Borrow);
        let str_ = self.cx.ty.str_;
        let sty = s.ty;
        if self.cx.ty.is_bottom(sty) {
            return self.error_expr(span);
        }
        let inner = self.cx.ty.opt_payload(sty).unwrap_or(sty);
        if inner == sty && self.cx.union_def(sty).is_none() {
            let tag = self.str_lit(self.cx.typeof_tag(sty), span);
            if is_place(&s) {
                return tag;
            }
            let stmt = hir::Stmt {
                kind: hir::StmtKind::Expr(s),
                span,
            };
            let block = hir::Block {
                stmts: vec![stmt],
                value: Some(Box::new(tag)),
                span,
            };
            return self.mk(H::Block(block), str_, span);
        }
        let mut arms = vec![];
        for (pat, t) in self.member_patterns(sty, span) {
            let tag = t.map_or("object", |t| self.cx.typeof_tag(t));
            let body = self.str_lit(tag, span);
            arms.push(hir::Arm {
                pat,
                guard: None,
                body,
            });
        }
        let kind = H::Match {
            scrutinee: Box::new(s),
            arms,
        };
        self.mk(kind, str_, span)
    }

    /// `typeof e === tag` (`negate`: `!==`).
    pub(crate) fn typeof_test(
        &mut self,
        operand: &ast::Expr,
        tag: &str,
        negate: bool,
        span: Span,
    ) -> hir::Expr {
        let s = self.expr(operand, None, Want::Borrow);
        if tag == "undefined" {
            self.cx.error(
                Diagnostic::error("`typeof` never returns \"undefined\" in Velt", span)
                    .with_note("`undefined` is not part of Velt: test for `null` (`x === null`)"),
            );
            return self.error_expr(span);
        }
        if !TYPEOF_TAGS.contains(&tag) {
            let tags: Vec<String> = TYPEOF_TAGS.iter().map(|t| format!("\"{t}\"")).collect();
            self.cx.error(
                Diagnostic::error(format!("`typeof` never returns \"{tag}\""), span)
                    .with_note(format!("`typeof` returns one of {}", tags.join(", "))),
            );
            return self.error_expr(span);
        }
        let pred = |cx: &Ctx, t: TyId| cx.typeof_tag(t) == tag;
        let test = self.type_test(s, &pred, tag == "object", span);
        if let Some(what) = test.never_msg {
            self.cx.err(
                format!("this `typeof` test is always false: no member of {what} is a \"{tag}\""),
                span,
            );
        }
        self.negated(test.expr, negate, span)
    }

    /// `e instanceof C`.
    pub(crate) fn instanceof(
        &mut self,
        e: &ast::Expr,
        ty: &ast::TypeExpr,
        span: Span,
    ) -> hir::Expr {
        let s = self.expr(e, None, Want::Borrow);
        let Some(class) = self.instanceof_class(ty) else {
            return self.error_expr(span);
        };
        let sty = s.ty;
        if self.cx.ty.is_bottom(sty) {
            return self.error_expr(span);
        }
        let inner = self.cx.ty.opt_payload(sty).unwrap_or(sty);
        let candidates = self.cx.union_members(inner).unwrap_or_else(|| vec![inner]);
        if !candidates.iter().any(|t| self.cx.class_of(*t).is_some()) {
            let tn = self.cx.display(sty);
            self.cx.err(
                format!("`instanceof` needs a class instance or a union with class members, found `{tn}`"),
                e.span,
            );
            return self.error_expr(span);
        }
        if let Some(base) = candidates
            .iter()
            .find(|t| self.is_downcast(**t, class))
            .copied()
        {
            self.downcast_error(base, class, span);
            return self.error_expr(span);
        }
        let pred = |cx: &Ctx, t: TyId| cx.is_instance_of(t, class);
        let test = self.type_test(s, &pred, false, span);
        if let Some(what) = test.never_msg {
            let cn = self.class_def_name(class);
            self.cx.err(
                format!("this `instanceof` test is always false: no member of {what} is a `{cn}`"),
                span,
            );
        }
        test.expr
    }

    /// Would testing a `t` for class `class` need a runtime downcast (`class` extends `t`'s)?
    fn is_downcast(&self, t: TyId, class: DefId) -> bool {
        let Some((d, _)) = self.cx.class_of(t) else {
            return false;
        };
        d != class && !self.cx.is_instance_of(t, class) && self.cx.class_extends(class, d)
    }

    fn downcast_error(&mut self, base: TyId, class: DefId, span: Span) {
        let (bn, cn) = (self.cx.display(base), self.class_def_name(class));
        self.cx.error(
            Diagnostic::error(
                format!("`instanceof {cn}` would need a downcast from `{bn}`"),
                span,
            )
            .with_note("downcasts are not supported; use a union of the subclasses (e.g. `Dog | Cat`) and narrow it")
            .with_note("or dispatch through an overridden method"),
        );
    }

    fn class_def_name(&self, d: DefId) -> String {
        self.cx.adt(d).map(|a| a.name.clone()).unwrap_or_default()
    }

    /// The class named after `instanceof` (reports errors).
    fn instanceof_class(&mut self, ty: &ast::TypeExpr) -> Option<DefId> {
        if let ast::TypeExprKind::Named { args, .. } = &ty.kind {
            if !args.is_empty() {
                self.cx.err(
                    "`instanceof` takes a class name without type arguments",
                    ty.span,
                );
                return None;
            }
        }
        if let Some(d) = self.instanceof_class_quiet(ty) {
            if let ast::TypeExprKind::Named { path, .. } = &ty.kind {
                self.cx.rec_item(path[0].span, Some(Item::Def(d)));
            }
            return Some(d);
        }
        let t = self.resolve(ty);
        if !self.cx.ty.is_bottom(t) {
            let tn = self.cx.display(t);
            self.cx.err(
                format!("`instanceof` needs a class, but `{tn}` is not one"),
                ty.span,
            );
        }
        None
    }

    /// The class named after `instanceof`, without diagnostics (for flow narrowing).
    pub(crate) fn instanceof_class_quiet(&self, ty: &ast::TypeExpr) -> Option<DefId> {
        let ast::TypeExprKind::Named { path, .. } = &ty.kind else {
            return None;
        };
        if path.len() == 1 && self.env.params.contains(&path[0].name) {
            return None;
        }
        match self.cx.lookup_path_at(self.module, path, ty.span)? {
            Item::Def(d)
                if self
                    .cx
                    .adt(d)
                    .is_some_and(|a| a.kind == hir::AdtKind::Class) =>
            {
                Some(d)
            }
            _ => None,
        }
    }

    /// `a == b` / `a != b` where exactly one side is a literal: a union on the other side is
    /// tested for that member and value.
    pub(crate) fn literal_eq(
        &mut self,
        lhs: &ast::Expr,
        rhs: &ast::Expr,
        negate: bool,
        span: Span,
    ) -> Option<LitEq> {
        use crate::body::narrow::literal_of;
        let (lit_first, lit) = match (literal_of(lhs), literal_of(rhs)) {
            (Some(l), None) => (true, l),
            (None, Some(l)) => (false, l),
            _ => return None,
        };
        let (lit_e, other_e) = if lit_first { (lhs, rhs) } else { (rhs, lhs) };
        let other = match self.discriminant_test(other_e, &lit, negate, span) {
            Ok(done) => return Some(LitEq::Done(done)),
            Err(Some(other)) => other,
            Err(None) => self.expr(other_e, None, Want::Borrow),
        };
        let inner = self.cx.ty.opt_payload(other.ty).unwrap_or(other.ty);
        if let Some(done) = self.impossible_literal(other_e, inner, &lit, span) {
            return Some(LitEq::Done(done));
        }
        if self.cx.union_def(inner).is_none() {
            let hint = (!self.cx.ty.is_bottom(other.ty)).then_some(other.ty);
            let l = self.expr(lit_e, hint, Want::Borrow);
            return Some(if lit_first {
                LitEq::Operands(l, other)
            } else {
                LitEq::Operands(other, l)
            });
        }
        let test = self.union_lit_test(other, inner, &lit, lit_e.span, span);
        Some(LitEq::Done(self.negated(test, negate, span)))
    }

    /// `x == lit` where `x` has a literal type (a narrowed discriminant, a `"box"` parameter)
    /// that `lit` can never equal: an error like TypeScript's "no overlap" (TS2367), since the
    /// branch it guards would see `x` as `never`.
    fn impossible_literal(
        &mut self,
        other_e: &ast::Expr,
        inner: TyId,
        lit: &ast::SignedLit,
        span: Span,
    ) -> Option<hir::Expr> {
        use crate::literals::{display_lit, lit_matches};
        let v = self.cx.lit_value(inner)?;
        let same_kind = matches!(
            (&v, &lit.lit),
            (hir::LitValue::Str(_), ast::Lit::Str(_))
                | (hir::LitValue::Bool(_), ast::Lit::Bool(_))
                | (hir::LitValue::Int(..), ast::Lit::Int { .. })
        );
        if !same_kind || lit_matches(&v, lit) {
            return None;
        }
        self.cx.error(
            Diagnostic::error(
                format!(
                    "this comparison is always false: `{}` is not a possible value of `{}`",
                    super::discriminated::show_lit(lit),
                    crate::body::switch::cases::source_text(other_e)
                ),
                span,
            )
            .with_note(format!("possible values: {}", display_lit(&v))),
        );
        Some(self.error_expr(span))
    }

    /// `u == lit` on a union (or nullable union) value `s` with union part `inner`.
    fn union_lit_test(
        &mut self,
        s: hir::Expr,
        inner: TyId,
        lit: &ast::SignedLit,
        lit_span: Span,
        span: Span,
    ) -> hir::Expr {
        let (v, m) = match self.lit_member(inner, lit) {
            Ok(x) => x,
            Err(msg) => {
                let un = self.cx.display(inner);
                self.cx.error(
                    Diagnostic::error("mismatched types", lit_span)
                        .with_note(format!("{msg} of `{un}`")),
                );
                return self.error_expr(span);
            }
        };
        let lit_pat = if self.cx.lit_value(m).is_some() {
            // A literal-type member is that one value.
            self.pat(P::Wildcard, m, lit_span)
        } else {
            let Some(l) = self.pat_lit(lit, m, lit_span) else {
                return self.error_expr(span);
            };
            self.pat(P::Lit(l), m, lit_span)
        };
        let Some((def, _)) = self.cx.union_def(inner) else {
            return self.error_expr(span);
        };
        let mut p = self.pat(
            P::Variant {
                def,
                variant: v,
                args: vec![lit_pat],
            },
            inner,
            span,
        );
        if s.ty != inner {
            p = self.pat(P::Some(Box::new(p)), s.ty, span);
        }
        self.bool_match(s, Some(p), span)
    }

    /// The member of union `u` a literal pattern / operand belongs to.
    pub(crate) fn lit_member(
        &mut self,
        u: TyId,
        lit: &ast::SignedLit,
    ) -> Result<(u32, TyId), String> {
        let members = self.cx.union_members(u).unwrap_or_default();
        // A literal-type member with this value wins over its base type.
        let exact = members.iter().position(|m| {
            self.cx
                .lit_value(*m)
                .is_some_and(|v| crate::literals::lit_matches(&v, lit))
        });
        if let Some(i) = exact {
            return Ok((i as u32, members[i]));
        }
        let ty = &self.cx.ty;
        let find = |f: &dyn Fn(TyId) -> bool| -> Vec<usize> {
            (0..members.len()).filter(|&i| f(members[i])).collect()
        };
        let (found, what) = match &lit.lit {
            ast::Lit::Str(_) => (find(&|t| t == ty.str_), "string"),
            ast::Lit::Bool(_) => (find(&|t| t == ty.bool_), "boolean"),
            ast::Lit::Int {
                suffix: Some(s), ..
            }
            | ast::Lit::Float {
                suffix: Some(s), ..
            } => {
                let st = self.cx.ty.primitive(s);
                (find(&|t| Some(t) == st), "number")
            }
            ast::Lit::Int { .. } => {
                let ints = find(&|t| ty.is_int(t));
                if ints.is_empty() {
                    (find(&|t| ty.is_float(t)), "number")
                } else {
                    (ints, "integer")
                }
            }
            ast::Lit::Float { .. } => (find(&|t| ty.is_float(t)), "float"),
            ast::Lit::Null => (vec![], "null"),
        };
        match found.as_slice() {
            [i] => Ok((*i as u32, members[*i])),
            [] => Err(format!("this {what} literal is not a member")),
            _ => Err(format!(
                "this {what} literal could be several members (add a type suffix like `5i32`)"
            )),
        }
    }

    /// [`lit_member`](Self::lit_member) without diagnostics.
    pub(crate) fn lit_member_quiet(&mut self, u: TyId, lit: ast::SignedLit) -> Option<(u32, TyId)> {
        self.lit_member(u, &lit).ok()
    }

    /// `match (s) { <members satisfying pred> => true, _ => false }`.
    fn type_test(
        &mut self,
        s: hir::Expr,
        pred: &dyn Fn(&Ctx, TyId) -> bool,
        null_matches: bool,
        span: Span,
    ) -> TypeTest {
        let sty = s.ty;
        let inner = self.cx.ty.opt_payload(sty).unwrap_or(sty);
        let is_union = self.cx.union_def(inner).is_some();
        let mut alts = vec![];
        for (p, t) in self.member_patterns(sty, span) {
            let yes = match t {
                None => null_matches,
                Some(t) => pred(self.cx, t),
            };
            if yes {
                alts.push(p);
            }
        }
        let never_msg =
            (is_union && alts.is_empty()).then(|| format!("`{}`", self.cx.display(sty)));
        let yes = match alts.len() {
            0 => None,
            1 => alts.pop(),
            _ => Some(self.pat(P::Or(alts), sty, span)),
        };
        let expr = self.bool_match(s, yes, span);
        TypeTest { expr, never_msg }
    }

    /// Patterns for each possible member of `sty` (`None` type: the `null` pattern).
    pub(crate) fn member_patterns(
        &mut self,
        sty: TyId,
        span: Span,
    ) -> Vec<(hir::Pat, Option<TyId>)> {
        let (inner, nullable) = match self.cx.ty.opt_payload(sty) {
            Some(p) => (p, true),
            None => (sty, false),
        };
        let mut out = vec![];
        if nullable {
            out.push((self.pat(P::None, sty, span), None));
        }
        let parts: Vec<(hir::Pat, TyId)> = match self.cx.union_def(inner) {
            Some((def, _)) => {
                let members = self.cx.union_members(inner).unwrap_or_default();
                members
                    .iter()
                    .enumerate()
                    .map(|(v, m)| (self.variant_pat(def, v as u32, *m, inner, span), *m))
                    .collect()
            }
            None => vec![(self.pat(P::Wildcard, inner, span), inner)],
        };
        for (p, t) in parts {
            let p = if nullable {
                self.pat(P::Some(Box::new(p)), sty, span)
            } else {
                p
            };
            out.push((p, Some(t)));
        }
        out
    }

    /// `Variant(_)` of union `u`.
    pub(crate) fn variant_pat(
        &self,
        def: DefId,
        v: u32,
        member: TyId,
        u: TyId,
        span: Span,
    ) -> hir::Pat {
        let wild = self.pat(P::Wildcard, member, span);
        self.pat(
            P::Variant {
                def,
                variant: v,
                args: vec![wild],
            },
            u,
            span,
        )
    }

    pub(crate) fn bool_match(
        &mut self,
        s: hir::Expr,
        yes: Option<hir::Pat>,
        span: Span,
    ) -> hir::Expr {
        let sty = s.ty;
        let b = self.cx.ty.bool_;
        let lit = |x: bool| hir::Expr {
            kind: H::Lit(hir::Lit::Bool(x)),
            ty: b,
            span,
        };
        let mut arms = vec![];
        if let Some(p) = yes {
            arms.push(hir::Arm {
                pat: p,
                guard: None,
                body: lit(true),
            });
        }
        arms.push(hir::Arm {
            pat: self.pat(P::Wildcard, sty, span),
            guard: None,
            body: lit(false),
        });
        let kind = H::Match {
            scrutinee: Box::new(s),
            arms,
        };
        self.mk(kind, b, span)
    }

    pub(super) fn negated(&self, e: hir::Expr, negate: bool, span: Span) -> hir::Expr {
        if !negate || self.cx.ty.is_bottom(e.ty) {
            return e;
        }
        let b = self.cx.ty.bool_;
        let kind = H::Unary {
            op: hir::UnOp::Not,
            expr: Box::new(e),
        };
        self.mk(kind, b, span)
    }

    /// Hint for using a member of a union (or nullable union) value without narrowing it.
    pub(crate) fn narrowing_note(&self, t: TyId) -> Option<String> {
        let inner = self.cx.ty.opt_payload(t).unwrap_or(t);
        self.cx.union_def(inner)?;
        Some(format!(
            "`{}` is a union: narrow it to one member first with `switch`, `typeof`, `instanceof` or `==`",
            self.cx.display(inner)
        ))
    }

    pub(crate) fn pat(&self, kind: P, ty: TyId, span: Span) -> hir::Pat {
        hir::Pat { kind, ty, span }
    }
}

/// A built type test and, if it can never be true on a union, the union's name.
struct TypeTest {
    expr: hir::Expr,
    never_msg: Option<String>,
}
