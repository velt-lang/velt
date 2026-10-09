//! What each `case` of a `switch` selects: literal cases select the slots (members) whose
//! discriminant / `typeof` tag / literal they name, `case null` the `null` slot, `case E.Member`
//! an enum member; any other case value is compared with `===` in a guard (`eq_values`).

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::cases::{source_text, strip_parens, Scrut, ScrutKind, Slot};
use crate::body::narrow::literal_of;
use crate::body::{FnCx, LocalKind, Want};
use crate::ctx::Item;
use crate::hir::{self, DefId, LitValue, PatKind as P, UseMode};
use crate::literals::lit_matches;
use crate::unions::TYPEOF_TAGS;

/// What one `case` selects.
pub(crate) struct Sel {
    pub pat: hir::Pat,
    pub guard: Option<hir::Expr>,
    /// Slots the case may select (for narrowing its body).
    pub touched: Vec<usize>,
    /// Slots every value of which the case selects (for coverage).
    pub covered: Vec<usize>,
}

impl FnCx<'_, '_> {
    /// What `case test:` selects.
    pub(super) fn case_sel(&mut self, s: &Scrut, test: &ast::Expr) -> Sel {
        let lit = literal_of(test).or_else(|| match s.kind {
            ScrutKind::Plain | ScrutKind::Enum(_) => None,
            _ => self.literal_const(test),
        });
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
            // A union value compares with any value `===` accepts (`case y:` with `y: "y"`).
            (ScrutKind::Union, None) => self.guard_sel(s, test),
            (ScrutKind::Discriminant(_) | ScrutKind::TypeOf, None) => self.key_guard_sel(s, test),
            (ScrutKind::Plain, None) => self.guard_sel(s, test),
        }
    }

    /// A constant of a literal type used as a case value: a local or module-level `const`
    /// initialized with a literal (`const y = "y"`) or a local of a literal type (`y: "y"`). It
    /// selects like the literal, as in TypeScript (it narrows and counts toward exhaustiveness).
    fn literal_const(&mut self, test: &ast::Expr) -> Option<ast::SignedLit> {
        let ast::ExprKind::Ident(id) = &strip_parens(test).kind else {
            return None;
        };
        let lit = match self.const_lit(id) {
            Some(l) => l,
            None => self.typed_lit(self.peek_local_ty(&id.name)?)?,
        };
        // Checked as a value too, so the constant counts as used (captures, editors).
        self.expr(test, None, Want::Borrow);
        Some(lit)
    }

    /// The literal a `const` named `id` was initialized with, when it has no type annotation.
    fn const_lit(&mut self, id: &ast::Ident) -> Option<ast::SignedLit> {
        if self.is_local_name(&id.name) {
            return self.peek_const_lit(&id.name);
        }
        let Some(Item::Def(d)) = self.cx.lookup_item_at(self.module, &id.name, id.span) else {
            return None;
        };
        let src = &self.cx.global(d)?.src;
        if src.ann.is_some() || src.owner.is_some() {
            return None;
        }
        literal_of(src.init?)
    }

    /// The value of literal type `t` as a pattern literal (not a float one).
    fn typed_lit(&self, t: crate::hir::TyId) -> Option<ast::SignedLit> {
        let lit = match self.cx.lit_value(t)? {
            LitValue::Str(v) => ast::SignedLit {
                lit: ast::Lit::Str(v),
                negative: false,
            },
            LitValue::Bool(b) => ast::SignedLit {
                lit: ast::Lit::Bool(b),
                negative: false,
            },
            LitValue::Int(_, v) => ast::SignedLit {
                lit: ast::Lit::Int {
                    value: v.unsigned_abs(),
                    suffix: None,
                },
                negative: v < 0,
            },
            LitValue::Float(..) => return None,
        };
        Some(lit)
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
        let lp = self.pat(P::Lit(lit), m, span);
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
            };
        }
        let Some(lit) = self.pat_lit(l, inner, span) else {
            return self.never_sel(s, span);
        };
        let mut pat = self.pat(P::Lit(lit), inner, span);
        if inner != sty {
            pat = self.pat(P::Some(Box::new(pat)), sty, span);
        }
        Sel {
            pat,
            guard: None,
            touched: vec![],
            covered: vec![],
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
        // A JS number (`switch (xs.length)`) stays one in the comparison.
        self.note_inferred_local(l, &s.expr);
        // Converted to the discriminant's type where it can be (`case 1:` on a `u8`); otherwise
        // the values compare like `===` does (`case t:` with `t: string | null`, #337).
        let v = self.expr(test, Some(ty), Want::Borrow);
        let v = self.try_coerce(v, ty).unwrap_or_else(|v| v);
        let cur = self.mk(hir::ExprKind::Local(l, mode), ty, span);
        let guard = self.eq_values(cur, v, span);
        Sel {
            pat: self.pat(P::Binding(l, mode), ty, span),
            guard: Some(guard),
            touched: (0..s.slots.len()).collect(),
            covered: vec![],
        }
    }

    /// `case v:` with a value that is not a literal on a discriminant (`switch (s.kind)`) or on
    /// `typeof x`, as TypeScript accepts it: the union value is bound, and its discriminant or
    /// tag (a `match` on the member) is compared with `v` in a guard. It narrows nothing.
    fn key_guard_sel(&mut self, s: &Scrut, test: &ast::Expr) -> Sel {
        let (ty, span) = (s.expr.ty, test.span);
        let mode = if self.cx.is_copy(ty) {
            UseMode::Copy
        } else {
            UseMode::Borrow
        };
        let l = self.new_local("<case>", ty, false, span, LocalKind::Bind);
        let cur = self.mk(hir::ExprKind::Local(l, mode), ty, span);
        let Some(key) = self.slot_key(s, cur, span) else {
            self.cx.err(
                format!(
                    "`case` values must be literals when switching on `{}`, whose values have different types",
                    s.what
                ),
                span,
            );
            self.expr(test, None, Want::Borrow);
            return self.never_sel(s, span);
        };
        let v = self.expr(test, Some(key.ty), Want::Borrow);
        let v = self.try_coerce(v, key.ty).unwrap_or_else(|v| v);
        let guard = self.eq_values(key, v, span);
        Sel {
            pat: self.pat(P::Binding(l, mode), ty, span),
            guard: Some(guard),
            touched: (0..s.slots.len()).collect(),
            covered: vec![],
        }
    }

    /// The discriminant or `typeof` tag of `cur` (a value of the scrutinee's type): a `match`
    /// over the slots. `None` when the discriminants have different types.
    fn slot_key(&mut self, s: &Scrut, cur: hir::Expr, span: Span) -> Option<hir::Expr> {
        let mut arms = vec![];
        for slot in &s.slots {
            let body = match (&s.kind, slot.member, slot.variant) {
                (ScrutKind::Discriminant(vals), _, Some(v)) => {
                    self.base_lit_expr(&vals[v as usize], span)
                }
                (ScrutKind::TypeOf, Some(m), _) => self.str_lit(self.cx.typeof_tag(m), span),
                (ScrutKind::TypeOf, None, _) => self.str_lit("object", span),
                _ => return None,
            };
            arms.push(hir::Arm {
                pat: slot.pat.clone(),
                guard: None,
                body,
            });
        }
        let ty = arms.first()?.body.ty;
        if arms.iter().any(|a| a.body.ty != ty) {
            return None;
        }
        if s.partial {
            // Members flow narrowing ruled out.
            let msg = self.str_lit("unreachable switch case", span);
            let never = self.cx.ty.never;
            arms.push(hir::Arm {
                pat: self.pat(P::Wildcard, s.expr.ty, span),
                guard: None,
                body: self.intrinsic(hir::Intrinsic::Panic, vec![msg], never, span),
            });
        }
        let m = hir::ExprKind::Match {
            scrutinee: Box::new(cur),
            arms,
        };
        Some(self.mk(m, ty, span))
    }

    /// `l === r` of two checked values (a `switch` case compared with its discriminant): the
    /// operands adapt as for `===` (a `T` next to a `T | null`, an inferred integer next to
    /// another number type, literal types as their base), and a mismatch is reported once.
    fn eq_values(&mut self, l: hir::Expr, r: hir::Expr, span: Span) -> hir::Expr {
        let (l, r) = self.nullable_operands(l, r);
        let (l, r) = if l.ty != r.ty {
            (self.widen_value(l), self.widen_value(r))
        } else {
            (l, r)
        };
        let (l, r) = self.mix_numbers(l, r);
        let (l, r) = self.mix_ints(l, r);
        let Some(t) = self.check_operands(ast::BinaryOp::Eq, l.ty, &r, span) else {
            return self.error_expr(span);
        };
        if !self.primitive_eq(t) {
            return self.structural_eq(l, r, false, span);
        }
        let kind = hir::ExprKind::Binary {
            op: hir::BinOp::Eq,
            lhs: Box::new(l),
            rhs: Box::new(r),
        };
        self.mk(kind, self.cx.ty.bool_, span)
    }

    /// A case that can never be selected (after an error).
    fn never_sel(&mut self, s: &Scrut, span: Span) -> Sel {
        let b = self.cx.ty.bool_;
        Sel {
            pat: self.pat(P::Wildcard, s.expr.ty, span),
            guard: Some(self.mk(hir::ExprKind::Lit(hir::Lit::Bool(false)), b, span)),
            touched: vec![],
            covered: vec![],
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
