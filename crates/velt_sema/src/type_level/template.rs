//! Template literal types: the strings they spell, and matching as patterns after `extends`;
//! `string & "a"`.

use velt_common::Span;
use velt_syntax::ast;

use super::{Binds, Subject, MAX_TEMPLATE_MEMBERS};
use crate::ctx::Ctx;
use crate::hir::{LitValue, Nothing, TyId, TyKind};
use crate::resolve::TyEnv;

impl Ctx<'_> {
    /// Does `s` match template literal pattern `` `a${X}b` ``?
    pub(super) fn match_template(
        &mut self,
        s: Subject,
        quasis: &[String],
        types: &[ast::TypeExpr],
        env: &TyEnv,
    ) -> Result<Option<Binds>, ()> {
        let Subject::Ty(st) = s else {
            return Ok(None);
        };
        let mut binds: Binds = vec![];
        for m in self.subjects_of(st) {
            let Subject::Ty(m) = m else {
                return Ok(None);
            };
            let Some(LitValue::Str(text)) = self.lit_value(m) else {
                return Ok(None);
            };
            let Some(found) = self.match_template_str(&text, quasis, types, env)? else {
                return Ok(None);
            };
            for (n, t) in found {
                match binds.iter_mut().find(|(b, _)| *b == n) {
                    Some((_, Some(prev))) if *prev != t => {
                        *prev = self.union_with(&[*prev, t], None, Span::default());
                    }
                    Some(_) => {}
                    None => binds.push((n, Some(t))),
                }
            }
        }
        Ok(Some(binds))
    }

    /// One string against a template literal pattern, as TypeScript matches it: each hole
    /// takes the text up to the next literal part (a single character when the next hole
    /// follows at once), the last one the rest.
    fn match_template_str(
        &mut self,
        text: &str,
        quasis: &[String],
        types: &[ast::TypeExpr],
        env: &TyEnv,
    ) -> Result<Option<Vec<(String, TyId)>>, ()> {
        let (first, last) = (&quasis[0], &quasis[quasis.len() - 1]);
        if !text.starts_with(first.as_str()) || text.len() < first.len() + last.len() {
            return Ok(None);
        }
        if !text[first.len()..].ends_with(last.as_str()) {
            return Ok(None);
        }
        let body = &text[first.len()..text.len() - last.len()];
        let mut pos = 0;
        let mut out = vec![];
        for (i, hole) in types.iter().enumerate() {
            let rest = &body[pos..];
            let end = if i + 1 == types.len() {
                rest.len()
            } else if quasis[i + 1].is_empty() {
                match rest.chars().next() {
                    Some(c) => c.len_utf8(),
                    None => return Ok(None),
                }
            } else {
                match rest.find(quasis[i + 1].as_str()) {
                    Some(e) => e,
                    None => return Ok(None),
                }
            };
            let piece = &rest[..end];
            pos += end
                + if i + 1 == types.len() {
                    0
                } else {
                    quasis[i + 1].len()
                };
            if let ast::TypeExprKind::Infer(n) = &hole.kind {
                let lit = self.lit_type(LitValue::Str(piece.to_string()));
                out.push((n.name.clone(), lit));
                continue;
            }
            let h = self.resolve_type(hole, env);
            let Some(h) = self.known_type(h, env, "a template literal type", hole.span) else {
                return Err(());
            };
            let lit = self.lit_type(LitValue::Str(piece.to_string()));
            let fits = h == self.ty.str_
                || (self.is_number_type(h) && is_numeric(piece))
                || self.relate(lit, h, &mut vec![])
                || self
                    .literal_strings(h)
                    .is_some_and(|ss| ss.iter().any(|x| x == piece));
            if !fits {
                return Ok(None);
            }
        }
        Ok(Some(out))
    }

    fn is_number_type(&self, t: TyId) -> bool {
        matches!(self.ty.kind(t), TyKind::Float(_) | TyKind::Int(_))
    }

    /// The strings a literal type or a union of literal types spells (`never`: none).
    pub(crate) fn literal_strings(&mut self, t: TyId) -> Option<Vec<String>> {
        if t == self.ty.never {
            return Some(vec![]);
        }
        let mut out = vec![];
        let (core, nothing) = match *self.ty.kind(t) {
            TyKind::Option(p, n) => (p, Some(n)),
            _ => (t, None),
        };
        let members = self.union_members(core).unwrap_or_else(|| vec![core]);
        for m in members {
            match self.lit_value(m)? {
                LitValue::Str(s) => out.push(s),
                LitValue::Bool(b) => out.push(b.to_string()),
                LitValue::Int(_, i) => out.push(i.to_string()),
                LitValue::Float(_, bits) => out.push(js_number(f64::from_bits(bits))),
            }
        }
        if let Some(n) = nothing {
            if n.has_null() {
                out.push("null".into());
            }
            if n != Nothing::Null {
                out.push("undefined".into());
            }
        }
        Some(out)
    }

    /// `` `on${K}` ``: the union of the strings it spells, its holes being literal types.
    pub(crate) fn resolve_template(
        &mut self,
        t: &ast::TypeExpr,
        quasis: &[String],
        types: &[ast::TypeExpr],
        env: &TyEnv,
    ) -> TyId {
        let mut acc = vec![quasis[0].clone()];
        for (i, hole) in types.iter().enumerate() {
            let h = self.resolve_type(hole, env);
            let Some(h) = self.known_type(h, env, "a template literal type", hole.span) else {
                return self.ty.error;
            };
            let Some(parts) = self.literal_strings(h) else {
                let shown = self.display(h);
                self.err(
                    format!("a template literal type with a `{shown}` hole is not supported yet outside the `extends` clause of a conditional type (#934)"),
                    hole.span,
                );
                return self.ty.error;
            };
            let mut next = Vec::with_capacity(acc.len() * parts.len());
            for a in &acc {
                for p in &parts {
                    next.push(format!("{a}{p}{}", quasis[i + 1]));
                }
            }
            if next.len() > MAX_TEMPLATE_MEMBERS {
                self.err(
                    format!(
                        "this template literal type has more than {MAX_TEMPLATE_MEMBERS} members"
                    ),
                    t.span,
                );
                return self.ty.error;
            }
            acc = next;
        }
        let lits: Vec<TyId> = acc
            .into_iter()
            .map(|s| self.lit_type(LitValue::Str(s)))
            .collect();
        match lits.as_slice() {
            [one] => *one,
            _ => self.union_of(&lits, false, t.span),
        }
    }
}

/// A JavaScript number as `String(n)` spells it (for the integers and short decimals literal
/// types hold).
fn js_number(n: f64) -> String {
    if n.is_finite() && n == n.trunc() && n.abs() < 1e21 {
        format!("{}", n as i128)
    } else {
        format!("{n}")
    }
}

/// Does `s` spell a number (`${number}` in a template literal pattern)?
fn is_numeric(s: &str) -> bool {
    !s.trim().is_empty() && s.trim() == s && s.parse::<f64>().is_ok()
}

impl Ctx<'_> {
    /// `string & K` with `K` a literal type or a union of them, and the other parts primitive
    /// types: the literals whose base is each of those primitives (TypeScript's `string & "a"`
    /// is `"a"`, `string & 1` is `never`). `None` for any other intersection.
    pub(crate) fn literal_meet(&mut self, operands: &[(TyId, Span)], span: Span) -> Option<TyId> {
        let is_prim = |cx: &Self, t: TyId| {
            matches!(
                cx.ty.kind(t),
                TyKind::Str | TyKind::Bool | TyKind::Int(_) | TyKind::Float(_)
            )
        };
        let mut lits: Option<Vec<TyId>> = None;
        let mut prims = vec![];
        for &(t, _) in operands {
            if is_prim(self, t) {
                prims.push(t);
                continue;
            }
            // `null` and `undefined` meet no primitive: `("a" | undefined) & string` is `"a"`.
            let t = self.ty.opt_payload(t).unwrap_or(t);
            let members = self.union_members(t).unwrap_or_else(|| vec![t]);
            if lits.is_some() || !members.iter().all(|m| self.lit_value(*m).is_some()) {
                return None;
            }
            lits = Some(members);
        }
        let lits = lits?;
        if prims.is_empty() {
            return None;
        }
        let kept: Vec<TyId> = lits
            .into_iter()
            .filter(|l| {
                let v = self.lit_value(*l).expect("ICE: literal member");
                let base = self.lit_base(&v);
                prims.iter().all(|p| *p == base)
            })
            .collect();
        Some(match kept.as_slice() {
            [one] => *one,
            _ => self.union_of(&kept, false, span),
        })
    }
}
