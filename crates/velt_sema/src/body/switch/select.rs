//! What each `case` of a `switch` selects: literal cases select the slots (members) whose
//! discriminant / `typeof` tag / literal they name, `case null` the `null` slot, `case E.Member`
//! an enum member; any other case value is compared with `==` in a guard.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::cases::{source_text, strip_parens, Scrut, ScrutKind, Slot};
use crate::body::narrow::literal_of;
use crate::body::{FnCx, LocalKind, Want};
use crate::ctx::Item;
use crate::hir::{self, DefId, PatKind as P, UseMode};
use crate::literals::lit_matches;
use crate::unions::TYPEOF_TAGS;

/// What one `case` selects.
pub(crate) struct Sel {
    pub pat: hir::Pat,
    pub guard: Option<hir::Expr>,
    /// Slots the case may select (for narrowing its body).
    pub touched: Vec<usize>,
    /// Slots every value of which the case selects (for exhaustiveness).
    pub covered: Vec<usize>,
    /// A literal the case compares with (duplicate detection).
    pub lit: Option<hir::Lit>,
}

impl FnCx<'_, '_> {
    /// What `case test:` selects.
    pub(super) fn case_sel(&mut self, s: &Scrut, test: &ast::Expr) -> Sel {
        let lit = literal_of(test);
        let null = matches!(strip_parens(test).kind, ast::ExprKind::Lit(ast::Lit::Null));
        match (&s.kind, lit) {
            (_, _) if null => self.null_sel(s, test.span),
            (ScrutKind::Discriminant(vals), Some(l)) => {
                let vals = vals.clone();
                let what = s.what.clone();
                self.slot_sel(s, test.span, &|_, slot| {
                    slot.variant
                        .is_some_and(|v| lit_matches(&vals[v as usize], &l))
                })
                .unwrap_or_else(|| {
                    let shown = source_text(test);
                    self.never_matches(
                        s,
                        format!("`{shown}` is not a possible value of `{what}`"),
                        test.span,
                    )
                })
            }
            (ScrutKind::TypeOf, Some(l)) => self.typeof_sel(s, &l, test.span),
            (ScrutKind::Union, Some(l)) => self.union_lit_sel(s, &l, test.span),
            (ScrutKind::Plain, Some(l)) => self.plain_lit_sel(s, &l, test.span),
            (ScrutKind::Enum(d), _) => match self.enum_member_of(*d, test) {
                Some(v) => self
                    .slot_sel(s, test.span, &|_, slot| slot.variant == Some(v))
                    .unwrap_or_else(|| self.guard_sel(s, test)),
                None => self.guard_sel(s, test),
            },
            (ScrutKind::Discriminant(_) | ScrutKind::TypeOf | ScrutKind::Union, None) => {
                self.cx.err(
                    format!(
                        "`case` values must be literals when switching on `{}`",
                        s.what
                    ),
                    test.span,
                );
                self.never_sel(s, test.span)
            }
            (ScrutKind::Plain, None) => self.guard_sel(s, test),
        }
    }

    /// The slots satisfying `pred` as one (or-)pattern; `None` if there are none.
    fn slot_sel(
        &mut self,
        s: &Scrut,
        span: Span,
        pred: &dyn Fn(&Self, &Slot) -> bool,
    ) -> Option<Sel> {
        let hits: Vec<usize> = (0..s.slots.len())
            .filter(|&k| pred(self, &s.slots[k]))
            .collect();
        if hits.is_empty() {
            return None;
        }
        let pats: Vec<hir::Pat> = hits.iter().map(|&k| s.slots[k].pat.clone()).collect();
        let pat = match pats.len() {
            1 => pats.into_iter().next().expect("ICE: one pattern"),
            _ => self.pat(P::Or(pats), s.expr.ty, span),
        };
        Some(Sel {
            pat,
            guard: None,
            touched: hits.clone(),
            covered: hits,
            lit: None,
        })
    }

    fn null_sel(&mut self, s: &Scrut, span: Span) -> Sel {
        if self.cx.ty.opt_payload(s.expr.ty).is_none() {
            let tn = self.cx.display(s.expr.ty);
            self.cx.err(
                format!("`case null` on a value of type `{tn}`, which is never null"),
                span,
            );
            return self.never_sel(s, span);
        }
        match self.slot_sel(s, span, &|_, slot| slot.member.is_none()) {
            Some(sel) => sel,
            None => Sel {
                pat: self.pat(P::None, s.expr.ty, span),
                guard: None,
                touched: vec![],
                covered: vec![],
                lit: Some(hir::Lit::Null),
            },
        }
    }

    fn typeof_sel(&mut self, s: &Scrut, l: &ast::SignedLit, span: Span) -> Sel {
        let tag = match &l.lit {
            ast::Lit::Str(t) if TYPEOF_TAGS.contains(&t.as_str()) => t.clone(),
            _ => {
                let tags: Vec<String> = TYPEOF_TAGS.iter().map(|t| format!("\"{t}\"")).collect();
                self.cx.error(
                    Diagnostic::error("`typeof` never returns this value", span)
                        .with_note(format!("`typeof` returns one of {}", tags.join(", "))),
                );
                return self.never_sel(s, span);
            }
        };
        let found = self.slot_sel(s, span, &|cx, slot| match slot.member {
            Some(m) => cx.cx.typeof_tag(m) == tag,
            None => tag == "object",
        });
        found.unwrap_or_else(|| {
            let tn = self.cx.display(s.expr.ty);
            self.never_matches(
                s,
                format!("this case never matches: no member of `{tn}` is a \"{tag}\""),
                span,
            )
        })
    }

    /// `case 5:` on a union value: the member the literal belongs to (a literal member is
    /// covered; a literal of a base-type member only tests that value).
    fn union_lit_sel(&mut self, s: &Scrut, l: &ast::SignedLit, span: Span) -> Sel {
        let sty = s.expr.ty;
        let inner = self.cx.ty.opt_payload(sty).unwrap_or(sty);
        let (v, m) = match self.lit_member(inner, l) {
            Ok(x) => x,
            Err(msg) => {
                let un = self.cx.display(inner);
                self.cx.error(
                    Diagnostic::error("mismatched types", span)
                        .with_note(format!("{msg} of `{un}`")),
                );
                return self.never_sel(s, span);
            }
        };
        if self.cx.lit_value(m).is_some() {
            if let Some(sel) = self.slot_sel(s, span, &|_, slot| slot.variant == Some(v)) {
                return sel;
            }
            return self.never_sel(s, span);
        }
        let Some(lit) = self.pat_lit(l, m, span) else {
            return self.never_sel(s, span);
        };
        let Some((def, _)) = self.cx.union_def(inner) else {
            return self.never_sel(s, span);
        };
        let lp = self.pat(P::Lit(lit.clone()), m, span);
        let mut pat = self.pat(
            P::Variant {
                def,
                variant: v,
                args: vec![lp],
            },
            inner,
            span,
        );
        if inner != sty {
            pat = self.pat(P::Some(Box::new(pat)), sty, span);
        }
        let touched = (0..s.slots.len())
            .filter(|&k| s.slots[k].variant == Some(v))
            .collect();
        Sel {
            pat,
            guard: None,
            touched,
            covered: vec![],
            lit: Some(lit),
        }
    }

    /// `case 5:` / `case "a":` on a plain (possibly nullable) value.
    fn plain_lit_sel(&mut self, s: &Scrut, l: &ast::SignedLit, span: Span) -> Sel {
        let sty = s.expr.ty;
        let inner = self.cx.ty.opt_payload(sty).unwrap_or(sty);
        if let Some(v) = self.cx.lit_value(inner) {
            if !lit_matches(&v, l) {
                let tn = self.cx.display(inner);
                self.cx.err(
                    format!("this case never matches a value of type `{tn}`"),
                    span,
                );
            }
            return Sel {
                pat: self.pat(P::Wildcard, sty, span),
                guard: None,
                touched: vec![],
                covered: vec![],
                lit: None,
            };
        }
        let Some(lit) = self.pat_lit(l, inner, span) else {
            return self.never_sel(s, span);
        };
        let mut pat = self.pat(P::Lit(lit.clone()), inner, span);
        if inner != sty {
            pat = self.pat(P::Some(Box::new(pat)), sty, span);
        }
        Sel {
            pat,
            guard: None,
            touched: vec![],
            covered: vec![],
            lit: Some(lit),
        }
    }

    /// `case expr:` compared with `==`: the scrutinee is bound and tested in a guard.
    fn guard_sel(&mut self, s: &Scrut, test: &ast::Expr) -> Sel {
        let (ty, span) = (s.expr.ty, test.span);
        let mode = if self.cx.is_copy(ty) {
            UseMode::Copy
        } else {
            UseMode::Borrow
        };
        let l = self.new_local("<case>", ty, false, span, LocalKind::Bind);
        let v = self.expr(test, Some(ty), Want::Borrow);
        let cur = self.mk(hir::ExprKind::Local(l, mode), ty, span);
        let guard = match self.case_operands(cur, v) {
            Some((cur, v)) => self.eq_values(cur, v, span),
            None => self.mk(
                hir::ExprKind::Lit(hir::Lit::Bool(false)),
                self.cx.ty.bool_,
                span,
            ),
        };
        Sel {
            pat: self.pat(P::Binding(l, mode), ty, span),
            guard: Some(guard),
            touched: (0..s.slots.len()).collect(),
            covered: vec![],
            lit: None,
        }
    }

    /// The scrutinee `cur` and case value `v` as operands of one `===` (#337): a `T | null`
    /// case on a `T` scrutinee compares the scrutinee converted to `T | null` (TypeScript
    /// accepts it; `null` matches no value), any other case value converts to the scrutinee's
    /// type. `None` after reporting a case value of another type (once: the comparison is not
    /// checked again).
    fn case_operands(&mut self, cur: hir::Expr, v: hir::Expr) -> Option<(hir::Expr, hir::Expr)> {
        let t = &self.cx.ty;
        let cur = if t.opt_payload(cur.ty).is_none() && t.opt_payload(v.ty).is_some() {
            match self.try_coerce(cur, v.ty) {
                Ok(cur) => return Some((cur, v)),
                Err(cur) => cur,
            }
        } else {
            cur
        };
        match self.try_coerce(v, cur.ty) {
            Ok(v) => Some((cur, v)),
            Err(v) => {
                self.report_mismatch(cur.ty, &v);
                None
            }
        }
    }

    /// A case that can never be selected (after an error).
    fn never_sel(&mut self, s: &Scrut, span: Span) -> Sel {
        let b = self.cx.ty.bool_;
        Sel {
            pat: self.pat(P::Wildcard, s.expr.ty, span),
            guard: Some(self.mk(hir::ExprKind::Lit(hir::Lit::Bool(false)), b, span)),
            touched: vec![],
            covered: vec![],
            lit: None,
        }
    }

    fn never_matches(&mut self, s: &Scrut, msg: String, span: Span) -> Sel {
        let possible: Vec<String> = s.slots.iter().map(|x| x.name.clone()).collect();
        let mut d = Diagnostic::error(msg, span);
        if !possible.is_empty() {
            d = d.with_note(format!("possible values: {}", possible.join(", ")));
        }
        self.cx.error(d);
        self.never_sel(s, span)
    }

    /// `E.Member` naming a member of enum `d`: its variant index.
    fn enum_member_of(&mut self, d: DefId, test: &ast::Expr) -> Option<u32> {
        let ast::ExprKind::Member {
            object,
            prop,
            optional: false,
        } = &strip_parens(test).kind
        else {
            return None;
        };
        let ast::ExprKind::Ident(id) = &object.kind else {
            return None;
        };
        if self.is_local_name(&id.name) || self.lookup_item(&id.name, id.span) != Some(Item::Def(d))
        {
            return None;
        }
        let e = self.cx.enum_info(d)?;
        let v = e.variants.iter().position(|v| v.name == prop.name)? as u32;
        self.cx
            .rec_ref(prop.span, crate::ide::record::Target::Variant(d, v));
        Some(v)
    }
}
