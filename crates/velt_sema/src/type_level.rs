//! Type-level evaluation (issue #934): conditional types with `infer`, mapped types, template
//! literal types, and the built-in `NonNullable`, `Exclude`, `Extract`, `Uppercase`,
//! `Lowercase`, `Capitalize` and `Uncapitalize`, as TypeScript evaluates them.
//!
//! Velt evaluates them where they are written, with a generic alias's arguments known: an alias
//! body is resolved once per use (`Ctx::expand_alias`), with `TyEnv::args` holding the
//! arguments, and alias arguments that are known types are passed on as they are
//! (`Ctx::item_type`), so `type A<T> = B<T>` evaluates `B` at `A`'s arguments. Names a type
//! binds (a mapped type's key, `infer X`, the member a distributive conditional is at) are
//! `TyEnv::locals`.
//!
//! A form whose argument is still a type parameter (in a generic function's signature, a generic
//! class's field) would need deferred evaluation, which is not implemented: it is a "not
//! supported yet" error, never a guess. Declaring such an alias is fine, as in TypeScript.
//!
//! Inside these forms an optional field's `T[K]` is `F | undefined` (`TyEnv::ts_optional`), as
//! TypeScript reads it, so `undefined extends T[K]` finds the optional keys; an optional field
//! of a mapped type's result drops that `undefined` again, and so does `-?`.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::anon::ShapeField;
use crate::ctx::Ctx;
use crate::hir::{AdtKind, LitValue, Nothing, TyId, TyKind};
use crate::resolve::TyEnv;
use crate::types::collect_params;

/// `infer X` while a pattern is matched is the placeholder `Param(INFER_BASE + i)`.
const INFER_BASE: u32 = 1 << 20;
/// `any` and `unknown` in a pattern: a placeholder that matches anything.
const WILDCARD: u32 = INFER_BASE - 1;
/// The most members a template literal type may expand to (TypeScript stops at 100 000).
const MAX_TEMPLATE_MEMBERS: usize = 10_000;

/// What a conditional type tests: a type, or `null` / `undefined` (which are not types alone).
#[derive(Clone, Copy, Debug)]
enum Subject {
    Ty(TyId),
    Nothing(Nothing),
}

/// What `infer X` found: a type, or nothing (TypeScript's `unknown`).
type Binds = Vec<(String, Option<TyId>)>;

impl Ctx<'_> {
    /// Does `t` mention a type parameter (pattern placeholders aside)?
    pub(crate) fn has_type_params(&self, t: TyId) -> bool {
        let mut ps = vec![];
        collect_params(&self.ty, t, &mut ps);
        ps.iter().any(|p| *p < WILDCARD)
    }

    /// `t`, resolved in `env`, with the alias's arguments substituted. `None` (reported, unless
    /// unused aliases are being checked) while it still mentions a type parameter.
    fn known_type(&mut self, t: TyId, env: &TyEnv, what: &str, span: Span) -> Option<TyId> {
        let t = self.subst(t, &env.args);
        if t == self.ty.error {
            return None;
        }
        if !self.has_type_params(t) {
            return Some(t);
        }
        if !self.checking_unused_aliases {
            let shown = self.display(t);
            self.error(
                Diagnostic::error(
                    format!("{what} over `{shown}`, which is not known here, is not supported yet"),
                    span,
                )
                .with_note("Velt evaluates conditional, mapped and template literal types where their type arguments are known; in generic code they would need deferred evaluation (#934)"),
            );
        }
        None
    }

    /// `C extends E ? T : F`.
    pub(crate) fn resolve_conditional(
        &mut self,
        t: &ast::TypeExpr,
        c: &ast::ConditionalType,
        env: &TyEnv,
    ) -> TyId {
        let mut env = env.clone();
        env.ts_optional = true;
        let env = &env;
        let naked = naked_param(&c.check, env);
        let subjects = match &c.check.kind {
            ast::TypeExprKind::Undefined => vec![Subject::Nothing(Nothing::Undefined)],
            ast::TypeExprKind::Null => vec![Subject::Nothing(Nothing::Null)],
            _ => {
                let ct = self.resolve_type(&c.check, env);
                let Some(ct) = self.known_type(ct, env, "a conditional type", c.check.span) else {
                    return self.ty.error;
                };
                match naked {
                    // A naked type parameter distributes over a union (`never` gives `never`).
                    Some(_) => self.subjects_of(ct),
                    None => vec![Subject::Ty(ct)],
                }
            }
        };
        let mut out = vec![];
        let mut nothing: Option<Nothing> = None;
        for s in subjects {
            let Ok(found) = self.match_pattern(s, &c.extends, env) else {
                return self.ty.error;
            };
            let (branch, binds) = match found {
                Some(b) => (&c.then, b),
                None => (&c.else_, vec![]),
            };
            let mut inner = env.clone();
            bind_all(&mut inner, binds);
            match (s, naked) {
                (Subject::Ty(st), Some(name)) => inner.locals.push((name.to_string(), st)),
                (Subject::Nothing(n), Some(name)) => {
                    if is_name(branch, name) {
                        nothing = Some(nothing.map_or(n, |m| m.join(n)));
                        continue;
                    }
                    if mentions(branch, name) {
                        self.err(
                            format!("`{name}` at `{}` in a conditional type's branch is not supported yet (#934)", n.word()),
                            branch.span,
                        );
                        return self.ty.error;
                    }
                }
                (_, None) => {}
            }
            let r = self.resolve_type(branch, &inner);
            if r == self.ty.error {
                return r;
            }
            out.push(r);
        }
        match (out.as_slice(), nothing) {
            ([one], None) => *one,
            _ => self.union_with(&out, nothing, t.span),
        }
    }

    /// The members a distributive conditional type tests one by one.
    fn subjects_of(&mut self, t: TyId) -> Vec<Subject> {
        let mut out = vec![];
        let (core, nothing) = match *self.ty.kind(t) {
            TyKind::Option(p, n) => (p, Some(n)),
            _ => (t, None),
        };
        if core != self.ty.never {
            match self.union_members(core) {
                Some(ms) => out.extend(ms.into_iter().map(Subject::Ty)),
                None => out.push(Subject::Ty(core)),
            }
        }
        match nothing {
            Some(Nothing::Both) => {
                out.push(Subject::Nothing(Nothing::Null));
                out.push(Subject::Nothing(Nothing::Undefined));
            }
            Some(n) => out.push(Subject::Nothing(n)),
            None => {}
        }
        out
    }

    /// Does `s` extend pattern `pat`? `Some` with what each `infer X` found; `Err` after an
    /// error.
    fn match_pattern(
        &mut self,
        s: Subject,
        pat: &ast::TypeExpr,
        env: &TyEnv,
    ) -> Result<Option<Binds>, ()> {
        match &pat.kind {
            ast::TypeExprKind::TemplateLiteral { quasis, types } => {
                return self.match_template(s, quasis, types, env);
            }
            ast::TypeExprKind::Undefined | ast::TypeExprKind::Null => {
                let want = match pat.kind {
                    ast::TypeExprKind::Null => Nothing::Null,
                    _ => Nothing::Undefined,
                };
                let ok = match s {
                    Subject::Nothing(n) => n == want,
                    Subject::Ty(t) => t == self.ty.never,
                };
                return Ok(ok.then(Vec::new));
            }
            _ => {}
        }
        let names = infer_names(pat);
        let mut inner = env.clone();
        inner.ts_optional = true;
        for (i, n) in names.iter().enumerate() {
            let p = self.ty.param(INFER_BASE + i as u32);
            inner.locals.push((n.clone(), p));
        }
        let wild = self.ty.param(WILDCARD);
        for n in ["any", "unknown"] {
            if !env.params.iter().any(|p| p == n) {
                inner.locals.push((n.to_string(), wild));
            }
        }
        let p = self.resolve_type(pat, &inner);
        if p == self.ty.error {
            return Err(());
        }
        let Some(p) = self.known_type(p, env, "a conditional type", pat.span) else {
            return Err(());
        };
        let mut slots = vec![None; names.len()];
        let ok = match s {
            Subject::Ty(t) => self.relate(t, p, &mut slots),
            Subject::Nothing(n) => self.relate_nothing(n, p),
        };
        Ok(ok.then(|| names.into_iter().zip(slots).collect()))
    }

    /// Does `null` / `undefined` extend `p`?
    fn relate_nothing(&self, n: Nothing, p: TyId) -> bool {
        match *self.ty.kind(p) {
            TyKind::Param(WILDCARD) => true,
            TyKind::Option(_, m) => n.within(m),
            _ => false,
        }
    }

    /// Is `s` assignable to `p` (TypeScript's relation, structural for object types), binding
    /// the `infer` placeholders `p` holds into `slots`?
    fn relate(&mut self, s: TyId, p: TyId, slots: &mut Vec<Option<TyId>>) -> bool {
        if s == p {
            return true;
        }
        let pk = self.ty.kind(p).clone();
        if let TyKind::Param(i) = pk {
            if i == WILDCARD {
                return true;
            }
            if i >= INFER_BASE {
                let slot = &mut slots[(i - INFER_BASE) as usize];
                *slot = Some(match *slot {
                    Some(prev) if prev != s => {
                        self.union_with(&[prev, s], None, Span::default())
                    }
                    _ => s,
                });
                return true;
            }
        }
        if s == self.ty.never || s == self.ty.error || p == self.ty.error {
            return true;
        }
        let sk = self.ty.kind(s).clone();
        if let TyKind::Option(sp, sn) = sk {
            let nothings: &[Nothing] = match sn {
                Nothing::Both => &[Nothing::Null, Nothing::Undefined],
                Nothing::Null => &[Nothing::Null],
                Nothing::Undefined => &[Nothing::Undefined],
            };
            return nothings.iter().all(|n| self.relate_nothing(*n, p) || self.binds_nothing(p))
                && self.relate(sp, p, slots);
        }
        if let Some(ms) = self.union_members(s) {
            return ms.into_iter().all(|m| self.relate(m, p, slots));
        }
        if let TyKind::Option(pp, _) = pk {
            return self.relate(s, pp, slots);
        }
        if let Some(ms) = self.union_members(p) {
            for m in ms {
                let saved = slots.clone();
                if self.relate(s, m, slots) {
                    return true;
                }
                *slots = saved;
            }
            return false;
        }
        match (sk, pk) {
            (TyKind::Literal(v), _) => self.lit_base(&v) == p,
            (TyKind::Array(a), TyKind::Array(b)) => self.relate(a, b, slots),
            (TyKind::Tuple(a), TyKind::Tuple(b)) => {
                a.len() == b.len() && a.iter().zip(&b).all(|(x, y)| self.relate(*x, *y, slots))
            }
            (TyKind::Tuple(a), TyKind::Array(b)) => a.iter().all(|x| self.relate(*x, b, slots)),
            (TyKind::Promise(a, _), TyKind::Promise(b, _)) | (TyKind::Shared(a), TyKind::Shared(b)) => {
                self.relate(a, b, slots)
            }
            (TyKind::Map(a1, a2), TyKind::Map(b1, b2)) => {
                self.relate(a1, b1, slots) && self.relate(a2, b2, slots)
            }
            (
                TyKind::FnPtr {
                    params: sp,
                    ret: sr,
                    ..
                },
                TyKind::FnPtr {
                    params: pp,
                    ret: pr,
                    ..
                },
            ) => {
                // Parameters are compared both ways (TypeScript's method bivariance), which
                // also lets `infer` find a parameter's type.
                sp.len() <= pp.len()
                    && sp.iter().zip(&pp).all(|(x, y)| {
                        let saved = slots.clone();
                        self.relate(*x, *y, slots) || {
                            *slots = saved;
                            self.relate(*y, *x, slots)
                        }
                    })
                    && self.relate(sr, pr, slots)
            }
            (_, TyKind::Adt(pd, _)) if self.is_structural_object(pd) => self.relate_fields(s, p, slots),
            (TyKind::Adt(sd, sa), TyKind::Adt(pd, pa)) | (TyKind::Dyn(sd, sa), TyKind::Dyn(pd, pa))
                if sd == pd && sa.len() == pa.len() =>
            {
                sa.iter().zip(&pa).all(|(x, y)| self.relate(*x, *y, slots))
            }
            _ => false,
        }
    }

    /// Is `p` an `infer` placeholder or the wildcard (which take `null` and `undefined` too)?
    fn binds_nothing(&self, p: TyId) -> bool {
        matches!(*self.ty.kind(p), TyKind::Param(i) if i >= WILDCARD)
    }

    /// An anonymous object type or a field-only interface's: related structurally.
    fn is_structural_object(&self, d: crate::hir::DefId) -> bool {
        self.adt(d)
            .is_some_and(|a| a.kind == AdtKind::Anon || self.field_only_of.contains_key(&d))
    }

    /// `s` has every field object type `p` requires, each assignable to `p`'s.
    fn relate_fields(&mut self, s: TyId, p: TyId, slots: &mut Vec<Option<TyId>>) -> bool {
        let (Some(pf), Some(sf)) = (self.plain_fields(p), self.plain_fields(s)) else {
            return false;
        };
        for f in pf {
            match sf.iter().find(|g| g.name == f.name) {
                Some(g) => {
                    let st = match g.optional && !f.optional {
                        true => self.ty.option_of(g.ty, Nothing::Undefined),
                        false => g.ty,
                    };
                    if !self.relate(st, f.ty, slots) {
                        return false;
                    }
                }
                None if f.optional => {}
                None => return false,
            }
        }
        true
    }

    /// The public fields of object type `t` (anonymous, a field-only interface, a class or a
    /// struct), without reporting anything when `t` has none.
    pub(crate) fn plain_fields(&mut self, t: TyId) -> Option<Vec<ShapeField>> {
        let TyKind::Adt(d, args) = self.ty.kind(t).clone() else {
            return None;
        };
        self.adt(d)?;
        let fields = self.fields_now(d).ok()?;
        Some(
            fields
                .into_iter()
                .filter(|(_, public)| *public)
                .map(|(f, _)| ShapeField {
                    ty: self.subst(f.ty, &args),
                    ..f
                })
                .collect(),
        )
    }

    /// Does `s` match template literal pattern `` `a${X}b` ``?
    fn match_template(
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
            pos += end + if i + 1 == types.len() { 0 } else { quasis[i + 1].len() };
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
                    format!("a template literal type with a `{shown}` hole (a pattern, not a union of strings) is supported only after `extends` in a conditional type"),
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
                    format!("this template literal type has more than {MAX_TEMPLATE_MEMBERS} members"),
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

    /// `{ readonly [K in S as N]?: V }`.
    pub(crate) fn resolve_mapped(
        &mut self,
        t: &ast::TypeExpr,
        m: &ast::MappedType,
        env: &TyEnv,
    ) -> TyId {
        let mut env = env.clone();
        env.ts_optional = true;
        let env = &env;
        // `[K in keyof X]` maps `X`'s fields, keeping their `?` and `readonly` (homomorphic).
        let source_fields = match &m.source.kind {
            ast::TypeExprKind::KeyOf(x) => {
                let xt = self.resolve_type(x, env);
                let Some(xt) = self.known_type(xt, env, "a mapped type", x.span) else {
                    return self.ty.error;
                };
                if matches!(self.ty.kind(xt), TyKind::Array(_) | TyKind::Tuple(_)) {
                    self.err(
                        "a mapped type over an array or tuple type is not supported yet (#934)",
                        x.span,
                    );
                    return self.ty.error;
                }
                self.plain_fields(xt)
            }
            _ => None,
        };
        let src = self.resolve_type(&m.source, env);
        let Some(src) = self.known_type(src, env, "a mapped type", m.source.span) else {
            return self.ty.error;
        };
        let Some(keys) = self.literal_strings(src) else {
            let shown = self.display(src);
            self.err(
                format!("a mapped type over `{shown}` (not a union of literal keys) is not supported yet; write `Record<{shown}, V>`"),
                m.source.span,
            );
            return self.ty.error;
        };
        let mut out: Vec<ShapeField> = vec![];
        for key in keys {
            let kt = self.lit_type(LitValue::Str(key.clone()));
            let mut inner = env.clone();
            inner.locals.push((m.param.name.clone(), kt));
            let names = match &m.name {
                Some(n) => {
                    let nt = self.resolve_type(n, &inner);
                    let Some(nt) = self.known_type(nt, env, "a mapped type's `as` clause", n.span)
                    else {
                        return self.ty.error;
                    };
                    match self.literal_strings(nt) {
                        Some(ns) => ns,
                        None => {
                            let shown = self.display(nt);
                            self.err(
                                format!("a mapped type's key must be a literal type; found `{shown}`"),
                                n.span,
                            );
                            return self.ty.error;
                        }
                    }
                }
                None => vec![key.clone()],
            };
            if names.is_empty() {
                continue;
            }
            let v = self.resolve_type(&m.value, &inner);
            if v == self.ty.error {
                return v;
            }
            let src_field = source_fields
                .as_ref()
                .and_then(|fs| fs.iter().find(|f| f.name == key));
            let was_optional = src_field.is_some_and(|f| f.optional);
            let optional = match m.optional {
                ast::MappedModifier::Add => true,
                ast::MappedModifier::Remove => false,
                ast::MappedModifier::Keep => was_optional,
            };
            let readonly = match m.readonly {
                ast::MappedModifier::Add => true,
                ast::MappedModifier::Remove => false,
                ast::MappedModifier::Keep => src_field.is_some_and(|f| f.readonly),
            };
            // An optional field's `undefined` is its absence; `-?` removes it as well.
            let v = match optional || was_optional {
                true => self.without_undefined(v),
                false => v,
            };
            for name in names {
                if out.iter().any(|f| f.name == name) {
                    continue;
                }
                out.push(ShapeField {
                    name,
                    ty: v,
                    readonly,
                    optional,
                });
            }
        }
        let ty = self.anon_type_with(&out, env.module);
        let spans = vec![t.span; out.len()];
        self.declare_anon_fields(ty, &spans);
        ty
    }

    /// `t` without `undefined` (`T | undefined` → `T`).
    fn without_undefined(&mut self, t: TyId) -> TyId {
        match *self.ty.kind(t) {
            TyKind::Option(p, Nothing::Undefined) => p,
            TyKind::Option(p, Nothing::Both) => self.ty.option_of(p, Nothing::Null),
            _ => t,
        }
    }

    /// The built-in type-level operators of this module, `None` for other names.
    pub(crate) fn resolve_type_level_builtin(
        &mut self,
        t: &ast::TypeExpr,
        name: &str,
        args: &[ast::TypeExpr],
        env: &TyEnv,
    ) -> Option<TyId> {
        let arity = match name {
            "NonNullable" | "Uppercase" | "Lowercase" | "Capitalize" | "Uncapitalize" => 1,
            "Exclude" | "Extract" => 2,
            _ => return None,
        };
        if args.len() != arity {
            self.err(
                format!(
                    "type `{name}` takes {arity} type argument(s) but {} were supplied",
                    args.len()
                ),
                t.span,
            );
            return Some(self.ty.error);
        }
        let mut env = env.clone();
        env.ts_optional = true;
        let env = &env;
        // `NonNullable<M>` of an `infer M` that found nothing (`unknown`) is `{}`.
        if name == "NonNullable" && unknown_name(&args[0], env) {
            return Some(self.anon_type_with(&[], env.module));
        }
        let a = self.resolve_type(&args[0], env);
        let Some(a) = self.known_type(a, env, &format!("`{name}`"), args[0].span) else {
            return Some(self.ty.error);
        };
        Some(match name {
            "NonNullable" => match *self.ty.kind(a) {
                TyKind::Option(p, _) => p,
                _ => a,
            },
            "Exclude" | "Extract" => {
                let keep_matching = name == "Extract";
                let mut kept = vec![];
                let mut nothing: Option<Nothing> = None;
                for s in self.subjects_of(a) {
                    let Ok(found) = self.match_pattern(s, &args[1], env) else {
                        return Some(self.ty.error);
                    };
                    if found.is_some() != keep_matching {
                        continue;
                    }
                    match s {
                        Subject::Ty(m) => kept.push(m),
                        Subject::Nothing(n) => nothing = Some(nothing.map_or(n, |m| m.join(n))),
                    }
                }
                match (kept.as_slice(), nothing) {
                    ([one], None) => *one,
                    _ => self.union_with(&kept, nothing, t.span),
                }
            }
            _ => {
                if a == self.ty.str_ {
                    return Some(a);
                }
                let Some(strs) = self.literal_strings(a) else {
                    let shown = self.display(a);
                    self.err(
                        format!("`{name}` needs a string literal type; found `{shown}`"),
                        args[0].span,
                    );
                    return Some(self.ty.error);
                };
                let lits: Vec<TyId> = strs
                    .iter()
                    .map(|s| {
                        let s = string_intrinsic(name, s);
                        self.lit_type(LitValue::Str(s))
                    })
                    .collect();
                match lits.as_slice() {
                    [one] => *one,
                    _ => self.union_of(&lits, false, t.span),
                }
            }
        })
    }
}

/// `Uppercase`, `Lowercase`, `Capitalize`, `Uncapitalize` of `s`.
fn string_intrinsic(name: &str, s: &str) -> String {
    let mut cs = s.chars();
    match name {
        "Uppercase" => s.to_uppercase(),
        "Lowercase" => s.to_lowercase(),
        "Capitalize" => match cs.next() {
            Some(c) => c.to_uppercase().chain(cs).collect(),
            None => String::new(),
        },
        _ => match cs.next() {
            Some(c) => c.to_lowercase().chain(cs).collect(),
            None => String::new(),
        },
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

/// The name of `t` when it is a naked type parameter (an alias's parameter, or a name a mapped
/// type or `infer` binds): a conditional type over it distributes over unions.
fn naked_param<'a>(t: &'a ast::TypeExpr, env: &TyEnv) -> Option<&'a str> {
    match &t.kind {
        ast::TypeExprKind::Named { path, args } if path.len() == 1 && args.is_empty() => {
            let n = path[0].name.as_str();
            (env.locals.iter().any(|(l, _)| l == n) || env.params.iter().any(|p| p == n))
                .then_some(n)
        }
        _ => None,
    }
}

/// Is `t` an `infer` name that found nothing (TypeScript's `unknown`)?
fn unknown_name(t: &ast::TypeExpr, env: &TyEnv) -> bool {
    match &t.kind {
        ast::TypeExprKind::Named { path, args } if path.len() == 1 && args.is_empty() => {
            env.unknowns.iter().any(|u| *u == path[0].name)
                && !env.locals.iter().any(|(l, _)| *l == path[0].name)
        }
        _ => false,
    }
}

/// Is `t` exactly the type named `name`?
fn is_name(t: &ast::TypeExpr, name: &str) -> bool {
    matches!(&t.kind, ast::TypeExprKind::Named { path, args }
        if path.len() == 1 && args.is_empty() && path[0].name == name)
}

/// Does `t` mention the type named `name` anywhere?
fn mentions(t: &ast::TypeExpr, name: &str) -> bool {
    struct Finder<'n> {
        name: &'n str,
        found: bool,
    }
    impl<'a> velt_syntax::visit::Visit<'a> for Finder<'_> {
        fn ty(&mut self, t: &'a ast::TypeExpr) {
            self.found |= matches!(&t.kind, ast::TypeExprKind::Named { path, .. }
                if path.len() == 1 && path[0].name == self.name);
        }
    }
    let mut f = Finder { name, found: false };
    velt_syntax::visit::walk_type_expr(t, &mut f);
    f.found
}

/// The names `infer X` binds in pattern `t`, in source order (not inside a nested conditional
/// type, whose own `infer`s are its own).
fn infer_names(t: &ast::TypeExpr) -> Vec<String> {
    fn go(t: &ast::TypeExpr, out: &mut Vec<String>) {
        use ast::TypeExprKind as K;
        match &t.kind {
            K::Infer(n) => {
                if !out.contains(&n.name) {
                    out.push(n.name.clone());
                }
            }
            K::Conditional(_) => {}
            K::Named { args, .. } => args.iter().for_each(|a| go(a, out)),
            K::Array(e) | K::KeyOf(e) | K::Construct(e) => go(e, out),
            K::Tuple(ts) | K::Union(ts) | K::Intersection(ts) => ts.iter().for_each(|a| go(a, out)),
            K::Indexed { object, key } => {
                go(object, out);
                go(key, out);
            }
            K::Function {
                params,
                ret,
                throws,
                ..
            } => {
                params.iter().for_each(|a| go(a, out));
                go(ret, out);
                if let Some(e) = throws {
                    go(e, out);
                }
            }
            K::GenericFunction { ty, .. } => go(ty, out),
            K::Object(fs) => fs.iter().for_each(|f| go(&f.ty, out)),
            K::IndexSig { fields, value, .. } => {
                fields.iter().for_each(|f| go(&f.ty, out));
                go(value, out);
            }
            K::Mapped(m) => go(&m.value, out),
            K::TemplateLiteral { types, .. } => types.iter().for_each(|a| go(a, out)),
            K::Predicate { .. }
            | K::Literal(_)
            | K::Null
            | K::Undefined
            | K::Void
            | K::Query(_) => {}
        }
    }
    let mut out = vec![];
    go(t, &mut out);
    out
}

/// Records what `infer` found in `env`.
fn bind_all(env: &mut TyEnv, binds: Binds) {
    for (n, b) in binds {
        match b {
            Some(t) => env.locals.push((n, t)),
            None => env.unknowns.push(n),
        }
    }
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
