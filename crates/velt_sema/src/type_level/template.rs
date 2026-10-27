//! Template literal types: the strings they spell, and matching as patterns after `extends`;
//! `string & "a"`.

use velt_common::Span;
use velt_syntax::ast;

use super::{Binds, Slots, Subject, MAX_TEMPLATE_MEMBERS};
use crate::ctx::Ctx;
use crate::hir::{LitValue, Nothing, TyId, TyKind};
use crate::resolve::TyEnv;

/// A template literal pattern's hole: `infer X`, or a type.
#[derive(Clone)]
enum Hole {
    Infer(String),
    Ty(TyId),
}

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
        // The holes, resolved once: `infer X`, or a type.
        let mut holes = Vec::with_capacity(types.len());
        for hole in types {
            holes.push(match &hole.kind {
                ast::TypeExprKind::Infer(n) => Hole::Infer(n.name.clone()),
                _ => {
                    let h = self.resolve_type(hole, env);
                    let Some(h) = self.known_type(h, env, "a template literal type", hole.span)
                    else {
                        return Err(());
                    };
                    Hole::Ty(h)
                }
            });
        }
        // Spelling out the holes of finitely many strings must stay within the limit.
        let mut spelled: usize = 1;
        for h in &holes {
            if let Hole::Ty(t) = h {
                let n = self.spelled_strings(*t, true).map_or(1, |ss| ss.len());
                spelled = spelled.saturating_mul(n.max(1));
            }
        }
        if spelled > MAX_TEMPLATE_MEMBERS {
            let span = types.first().map_or(Span::default(), |t| t.span);
            self.err(
                format!("this template literal type has more than {MAX_TEMPLATE_MEMBERS} members"),
                span,
            );
            return Err(());
        }
        let mut binds: Binds = vec![];
        for m in self.subjects_of(st) {
            let Subject::Ty(m) = m else {
                return Ok(None);
            };
            let Some(LitValue::Str(text)) = self.lit_value(m) else {
                return Ok(None);
            };
            let Some(found) = self.match_template_str(&text, quasis.to_vec(), holes.clone()) else {
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

    /// One string against a template literal pattern, as TypeScript matches it: a hole of
    /// finitely many strings (a literal type, `boolean`) is each of them spelled out (TypeScript
    /// normalizes `` `${boolean}x` `` to `"truex" | "falsex"`); then each hole takes the text up
    /// to the next literal part (a single character when the next hole follows at once), the
    /// last one the rest.
    fn match_template_str(
        &mut self,
        text: &str,
        quasis: Vec<String>,
        holes: Vec<Hole>,
    ) -> Option<Vec<(String, TyId)>> {
        let finite = holes.iter().enumerate().find_map(|(i, h)| match h {
            Hole::Ty(t) => self.spelled_strings(*t, true).map(|ss| (i, ss)),
            Hole::Infer(_) => None,
        });
        if let Some((j, alternatives)) = finite {
            for alt in alternatives {
                let mut q = quasis[..j].to_vec();
                q.push(format!("{}{alt}{}", quasis[j], quasis[j + 1]));
                q.extend_from_slice(&quasis[j + 2..]);
                let mut hs = holes.clone();
                hs.remove(j);
                if let Some(found) = self.match_template_str(text, q, hs) {
                    return Some(found);
                }
            }
            return None;
        }
        if holes.is_empty() {
            return (text == quasis[0]).then(Vec::new);
        }
        let (first, last) = (&quasis[0], &quasis[quasis.len() - 1]);
        if !text.starts_with(first.as_str()) || text.len() < first.len() + last.len() {
            return None;
        }
        if !text[first.len()..].ends_with(last.as_str()) {
            return None;
        }
        let body = &text[first.len()..text.len() - last.len()];
        let mut pos = 0;
        let mut out = vec![];
        for (i, hole) in holes.iter().enumerate() {
            let rest = &body[pos..];
            let end = if i + 1 == holes.len() {
                rest.len()
            } else if quasis[i + 1].is_empty() {
                rest.chars().next()?.len_utf8()
            } else {
                rest.find(quasis[i + 1].as_str())?
            };
            let piece = &rest[..end];
            pos += end
                + if i + 1 == holes.len() {
                    0
                } else {
                    quasis[i + 1].len()
                };
            let lit = self.lit_type(LitValue::Str(piece.to_string()));
            let h = match hole {
                Hole::Infer(n) => {
                    out.push((n.clone(), lit));
                    continue;
                }
                Hole::Ty(h) => *h,
            };
            if !self.hole_fits(piece, lit, h) {
                return None;
            }
        }
        Some(out)
    }

    /// Does `piece` (as literal type `lit`) fit a hole of type `h`?
    fn hole_fits(&mut self, piece: &str, lit: TyId, h: TyId) -> bool {
        let core = self.ty.core(h).unwrap_or(h);
        let members = self.union_members(core).unwrap_or_else(|| vec![core]);
        members.iter().any(|m| {
            *m == self.ty.str_
                || (self.is_number_type(*m) && is_numeric(piece))
                || (*m == self.ty.bool_ && (piece == "true" || piece == "false"))
        }) || self.relate(lit, h, &mut Slots::default())
            || self
                .spelled_strings(h, true)
                .is_some_and(|ss| ss.iter().any(|x| x == piece))
    }

    fn is_number_type(&self, t: TyId) -> bool {
        matches!(self.ty.kind(t), TyKind::Float(_) | TyKind::Int(_))
    }

    /// The strings a literal type or a union of literal types spells (`never`: none).
    pub(crate) fn literal_strings(&mut self, t: TyId) -> Option<Vec<String>> {
        self.spelled_strings(t, false)
    }

    /// [`Self::literal_strings`], with `boolean` as `"true" | "false"` when `bools` (a
    /// template literal type's hole).
    fn spelled_strings(&mut self, t: TyId, bools: bool) -> Option<Vec<String>> {
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
            if bools && m == self.ty.bool_ {
                out.extend(["true".to_string(), "false".to_string()]);
                continue;
            }
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

    /// The keys a mapped type's `in` clause spells, each with its type: a string literal key is
    /// itself, a number literal key stays a number (`{ [K in 1]: K }` is `{ 1: 1 }`).
    pub(crate) fn literal_keys(&mut self, t: TyId) -> Option<Vec<(String, TyId)>> {
        let names = self.literal_strings(t)?;
        if t == self.ty.never {
            return Some(vec![]);
        }
        let core = self.ty.core(t).unwrap_or(t);
        let members = self.union_members(core).unwrap_or_else(|| vec![core]);
        Some(
            names
                .into_iter()
                .enumerate()
                .map(|(i, name)| {
                    let ty = match members.get(i).and_then(|m| self.lit_value(*m)) {
                        Some(LitValue::Int(..) | LitValue::Float(..)) => members[i],
                        _ => self.lit_type(LitValue::Str(name.clone())),
                    };
                    (name, ty)
                })
                .collect(),
        )
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
            let Some(parts) = self.spelled_strings(h, true) else {
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

pub(crate) use velt_syntax::js_number;

/// Does `s` spell a finite number as JavaScript's `Number(s)` reads it (`${number}` in a
/// template literal pattern; TypeScript's `isValidNumberString`)? Surrounding white space,
/// `0x` / `0o` / `0b` and a sign are allowed; `""`, `NaN` and `Infinity` are not.
fn is_numeric(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let t = s.trim_matches(is_js_space);
    if t.is_empty() {
        return true; // `Number("  ")` is 0
    }
    for (prefix, radix) in [
        ("0x", 16),
        ("0X", 16),
        ("0o", 8),
        ("0O", 8),
        ("0b", 2),
        ("0B", 2),
    ] {
        if let Some(d) = t.strip_prefix(prefix) {
            return !d.is_empty() && d.chars().all(|c| c.is_digit(radix));
        }
    }
    let u = t.strip_prefix(['+', '-']).unwrap_or(t);
    // StrUnsignedDecimalLiteral: digits [. digits] [e [sign] digits], or . digits [exponent].
    let (mantissa, exp) = match u.find(['e', 'E']) {
        Some(i) => (&u[..i], Some(&u[i + 1..])),
        None => (u, None),
    };
    let (int, frac) = match mantissa.split_once('.') {
        Some((a, b)) => (a, Some(b)),
        None => (mantissa, None),
    };
    let digits = |x: &str| x.chars().all(|c| c.is_ascii_digit());
    let mantissa_ok = digits(int)
        && frac.is_none_or(digits)
        && !(int.is_empty() && frac.is_none_or(str::is_empty));
    let exp_ok = exp.is_none_or(|e| {
        let e = e.strip_prefix(['+', '-']).unwrap_or(e);
        !e.is_empty() && digits(e)
    });
    // A literal too large for a double is `Infinity`, which is not finite.
    mantissa_ok && exp_ok && t.parse::<f64>().is_ok_and(f64::is_finite)
}

/// JavaScript's white space and line terminators (what `Number(s)` trims).
fn is_js_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
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
            let t = self.ty.core(t).unwrap_or(t);
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

#[cfg(test)]
mod tests {
    use super::{is_numeric, js_number};

    #[test]
    fn numbers_print_as_javascript_does() {
        for (n, s) in [
            (1e21, "1e+21"),
            (1.5e-7, "1.5e-7"),
            (1.2345678901234568e20, "123456789012345680000"),
            (0.000001, "0.000001"),
            (2.5, "2.5"),
            (-0.5, "-0.5"),
            (100.0, "100"),
            (1.25e300, "1.25e+300"),
            (5e-324, "5e-324"),
        ] {
            assert_eq!(js_number(n), s);
        }
    }

    #[test]
    fn numeric_strings_read_as_javascript_does() {
        for s in [
            " 12", "12 ", "0x1F", "0b11", "0o7", "1e3", ".5", "5.", "+1", "-0", "  ",
        ] {
            assert!(is_numeric(s), "{s:?}");
        }
        for s in [
            "", "Infinity", "NaN", "inf", "1_0", "0x", "-0x1", "1e", ".", "1e400", "1.2.3",
        ] {
            assert!(!is_numeric(s), "{s:?}");
        }
    }
}
