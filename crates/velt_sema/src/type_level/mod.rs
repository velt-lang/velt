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
use crate::hir::{LitValue, Nothing, TyId, TyKind, TyOp};
use crate::resolve::TyEnv;
use crate::types::collect_params;

pub(crate) mod instances;
pub(crate) mod key_kinds;
mod relate;
mod template;

use relate::Slots;
pub(crate) use template::js_number;

/// `infer X` while a pattern is matched is the placeholder `Param(INFER_BASE + i)`.
pub(super) const INFER_BASE: u32 = 1 << 20;
/// `any` in a pattern: a placeholder that matches anything and is assignable to anything.
pub(super) const WILDCARD: u32 = INFER_BASE - 1;
/// `unknown` in a pattern: matches anything, and is assignable only to `any` and `unknown`
/// (in a function type's parameter).
pub(super) const UNKNOWN: u32 = INFER_BASE - 2;
/// The most members a template literal type may expand to (TypeScript stops at 100 000).
pub(super) const MAX_TEMPLATE_MEMBERS: usize = 10_000;

/// The state of type-level evaluation, kept in `Ctx`.
#[derive(Default)]
pub(crate) struct State {
    /// While an alias is expanded: the outermost use of an alias, as written, where a
    /// type-level form that can't be evaluated is reported.
    pub(crate) alias_site: Option<Span>,
    /// While a call's argument is checked against its parameter's type: the argument, where a
    /// deferred intersection that can't be reduced at these type arguments is reported.
    pub(crate) use_site: Option<Span>,
    /// How object types' properties whose names spell numbers were named (`key_kinds`).
    pub(crate) key_kinds:
        std::collections::HashMap<(crate::hir::DefId, String), key_kinds::KeyKind>,
    /// How many alias expansions in progress have an argument that is a type parameter: a form
    /// that needs that argument defers the alias (`TyKind::Op`) instead of failing.
    pub(crate) deferring: u32,
    /// The alias being expanded needs an argument that is still a type parameter.
    pub(crate) stuck: bool,
    /// Where each deferred type was first written (reductions report there).
    pub(crate) op_sites: std::collections::HashMap<TyId, Span>,
    /// The sites of the deferred intersections (`op_sites`) one reduction of which was
    /// reported: a reduction at an instantiation, with no use to report at, is not reported
    /// again.
    pub(crate) failed_sites: std::collections::HashSet<Span>,
    /// The module each deferred intersection was written in (its object type is made there).
    pub(crate) op_modules: std::collections::HashMap<TyId, usize>,
    /// The parts of each object type an intersection made (`A & { id: string }`), so inference
    /// through `T & { id: string }` binds `T` to `A`, as TypeScript does (`Ctx::match_ty`).
    pub(crate) intersection_parts: std::collections::HashMap<TyId, Vec<TyId>>,
    /// Each deferred type with concrete arguments, reduced (`hir::Program::type_ops`).
    pub(crate) reduced: std::collections::HashMap<TyId, TyId>,
}

/// The deferred types lowering may meet, with what they reduce to (`hir::Program::type_ops`):
/// every reduction sema made, after readonly erasure. [`instances::reach`] has reduced each one
/// an instantiation of a generic definition reaches.
pub(crate) fn lowered_ops(cx: &mut Ctx) -> std::collections::HashMap<TyId, TyId> {
    std::mem::take(&mut cx.type_level.reduced)
}

/// What a conditional type tests: a type, or `null` / `undefined` (which are not types alone).
#[derive(Clone, Copy, Debug)]
pub(super) enum Subject {
    Ty(TyId),
    Nothing(Nothing),
}

/// What `infer X` found: a type, or nothing (TypeScript's `unknown`).
pub(super) type Binds = Vec<(String, Option<TyId>)>;

impl Ctx<'_> {
    /// Does `t` mention a type parameter (pattern placeholders aside)?
    pub(crate) fn has_type_params(&self, t: TyId) -> bool {
        let mut ps = vec![];
        collect_params(&self.ty, t, &mut ps);
        ps.iter().any(|p| *p < UNKNOWN)
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
        if !self.defer_param_form() {
            // At the use of the alias whose body has the form, where the type parameter is.
            let span = self.type_level.alias_site.unwrap_or(span);
            let msg = format!("{what} over a type parameter is not supported yet");
            self.error(
                Diagnostic::error(msg, span)
                .with_note("Velt evaluates conditional, mapped and template literal types where their type arguments are known; in generic code they would need deferred evaluation (#934)"),
            );
        }
        None
    }

    /// A deferred type as written: `PropsAccessor<TProps>` (`names`: as for `display_in`).
    pub(crate) fn display_op(&self, o: TyOp, args: &[TyId], names: &[String]) -> String {
        let args: Vec<String> = args.iter().map(|t| self.display_in(*t, names)).collect();
        let TyOp::Alias(a) = o else {
            return args.join(" & ");
        };
        format!(
            "{}<{}>",
            self.aliases[a as usize].decl.name.name,
            args.join(", ")
        )
    }

    /// Does `t` hold a deferred type (`TyKind::Op`)?
    pub(crate) fn mentions_op(&self, t: TyId) -> bool {
        let mut found = false;
        self.ty
            .visit(t, &mut |k| found |= matches!(k, TyKind::Op(..)));
        found
    }

    /// A form needs a type argument that is still a type parameter. Inside an alias expanded
    /// with such an argument, the alias is deferred (`true`: nothing to report); `true` also
    /// while unused aliases are checked on their own. `false`: report the form.
    pub(crate) fn defer_param_form(&mut self) -> bool {
        if self.type_level.deferring > 0 {
            self.type_level.stuck = true;
            return true;
        }
        self.checking_unused_aliases
    }

    /// The deferred application of alias `a` to `args` (which mention type parameters), as
    /// written at `span`.
    pub(crate) fn deferred_alias(&mut self, a: u32, args: Vec<TyId>, span: Span) -> TyId {
        let op = self.ty.intern(TyKind::Op(TyOp::Alias(a), args));
        self.type_level.op_sites.entry(op).or_insert(span);
        op
    }

    /// Deferred type `t`, whose arguments are now concrete, evaluated (memoized); `Error`, with
    /// no second report, when an argument is one.
    pub(crate) fn reduce_op(&mut self, t: TyId) -> TyId {
        if let Some(&r) = self.type_level.reduced.get(&t) {
            return r;
        }
        let TyKind::Op(o, args) = self.ty.kind(t).clone() else {
            return t;
        };
        if args.iter().any(|x| self.ty.has_error(*x)) {
            return self.ty.error;
        }
        let TyOp::Alias(a) = o else {
            // At the argument whose expected type needs it, else where it was written.
            let span = self
                .type_level
                .alias_site
                .or(self.type_level.use_site)
                .or_else(|| self.type_level.op_sites.get(&t).copied())
                .unwrap_or(Span::DUMMY);
            let written = self.type_level.op_sites.get(&t).copied();
            // Reduced where it was written (at an instantiation), after a use of it with other
            // type arguments failed: reported once, at that use.
            let unplaced =
                self.type_level.alias_site.is_none() && self.type_level.use_site.is_none();
            let quiet =
                unplaced && written.is_some_and(|w| self.type_level.failed_sites.contains(&w));
            let before = self.diags.len();
            let r = self.reduce_intersection(t, args, span);
            if quiet {
                self.diags.truncate(before);
            } else if let Some(w) = written.filter(|_| self.diags.len() > before) {
                self.type_level.failed_sites.insert(w);
            }
            self.type_level.reduced.insert(t, r);
            self.link_part_ifaces(r);
            return r;
        };
        let span = self
            .type_level
            .alias_site
            .or_else(|| self.type_level.op_sites.get(&t).copied())
            .unwrap_or(self.aliases[a as usize].decl.name.span);
        let r = self.alias_body(a, args, span);
        self.type_level.reduced.insert(t, r);
        r
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
        self.rejoin_booleans(&mut out);
        match (out.as_slice(), nothing) {
            ([one], None) => *one,
            _ => self.union_with(&out, nothing, t.span),
        }
    }

    /// `true` and `false` both among `ts` are `boolean` again.
    fn rejoin_booleans(&mut self, ts: &mut Vec<TyId>) {
        let t = self.lit_type(LitValue::Bool(true));
        let f = self.lit_type(LitValue::Bool(false));
        if let (Some(i), true) = (
            ts.iter().position(|x| *x == t || *x == f),
            ts.contains(&t) && ts.contains(&f),
        ) {
            ts[i] = self.ty.bool_;
            ts.retain(|x| *x != t && *x != f);
        }
    }

    /// The members a distributive conditional type tests one by one (`boolean` is
    /// `true | false`).
    fn subjects_of(&mut self, t: TyId) -> Vec<Subject> {
        let mut out = vec![];
        let (core, nothing) = match *self.ty.kind(t) {
            TyKind::Option(p, n) => (p, Some(n)),
            _ => (t, None),
        };
        if core != self.ty.never {
            let ms = self.union_members(core).unwrap_or_else(|| vec![core]);
            for m in ms {
                if m == self.ty.bool_ {
                    for b in [true, false] {
                        out.push(Subject::Ty(self.lit_type(LitValue::Bool(b))));
                    }
                } else {
                    out.push(Subject::Ty(m));
                }
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
        for (n, w) in [("any", WILDCARD), ("unknown", UNKNOWN)] {
            if !env.params.iter().any(|p| p == n) {
                let w = self.ty.param(w);
                inner.locals.push((n.to_string(), w));
            }
        }
        let p = self.resolve_type(pat, &inner);
        if p == self.ty.error {
            return Err(());
        }
        let Some(p) = self.known_type(p, env, "a conditional type", pat.span) else {
            return Err(());
        };
        let mut slots = Slots::new(names.len());
        let ok = match s {
            Subject::Ty(t) => self.relate(t, p, &mut slots),
            Subject::Nothing(n) => self.relate_nothing(n, p),
        };
        if !ok {
            return Ok(None);
        }
        let found = self.finish_slots(slots, env.module, pat.span)?;
        Ok(Some(names.into_iter().zip(found).collect()))
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
                // Over a type parameter, a homomorphic mapped type distributes over a union
                // (`null` and `undefined` stay) and gives a primitive as it is.
                if let Some(name) = naked_param(x, env) {
                    if let Some(r) = self.distribute_mapped(t, m, env, name, xt) {
                        return r;
                    }
                }
                if matches!(self.ty.kind(xt), TyKind::Array(_) | TyKind::Tuple(_)) {
                    self.err(
                        "a mapped type over an array or tuple type is not supported yet (#934)",
                        x.span,
                    );
                    return self.ty.error;
                }
                self.relatable_fields(xt)
            }
            _ => None,
        };
        let src = self.resolve_type(&m.source, env);
        let Some(src) = self.known_type(src, env, "a mapped type", m.source.span) else {
            return self.ty.error;
        };
        let Some(keys) = self.literal_keys(src) else {
            let shown = self.display(src);
            self.err(
                format!("a mapped type over `{shown}` (not a union of literal keys) is not supported yet; write `Record<{shown}, V>`"),
                m.source.span,
            );
            return self.ty.error;
        };
        let mut out: Vec<ShapeField> = vec![];
        let mut key_tys: Vec<(String, TyId)> = vec![];
        for (key, kt) in keys {
            let mut inner = env.clone();
            inner.locals.push((m.param.name.clone(), kt));
            let names: Vec<(String, TyId)> = match &m.name {
                Some(n) => {
                    let nt = self.resolve_type(n, &inner);
                    let Some(nt) = self.known_type(nt, env, "a mapped type's `as` clause", n.span)
                    else {
                        return self.ty.error;
                    };
                    match self.literal_keys(nt) {
                        Some(ns) => ns,
                        None => {
                            let shown = self.display(nt);
                            self.err(
                                format!(
                                    "a mapped type's key must be a literal type; found `{shown}`"
                                ),
                                n.span,
                            );
                            return self.ty.error;
                        }
                    }
                }
                None => vec![(key.clone(), kt)],
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
            for (name, name_ty) in names {
                key_tys.push((name.clone(), name_ty));
                // Keys the `as` clause maps together have the union of the values, and the
                // first one's modifiers.
                let existing = out.iter().position(|f| f.name == name);
                let field_optional = existing.map_or(optional, |i| out[i].optional);
                // An optional field's `undefined` is its absence; `-?` removes it as well.
                let v = match field_optional || m.optional == ast::MappedModifier::Remove {
                    true => self.without_undefined(v),
                    false => v,
                };
                if let Some(i) = existing {
                    let prev = out[i].ty;
                    out[i].ty = self.union_with(&[prev, v], None, t.span);
                    if out[i].ty == self.ty.error {
                        return self.ty.error;
                    }
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
        // Integer keys come first, in ascending order, as JavaScript orders an object's keys.
        out.sort_by_key(|f| array_index(&f.name).map_or((1, 0), |i| (0, i)));
        let ty = self.anon_type_with(&out, env.module);
        let spans = vec![t.span; out.len()];
        self.declare_anon_fields(ty, &spans);
        for (name, key) in key_tys {
            self.record_key_kind(ty, &name, key);
        }
        ty
    }

    /// `{ [K in keyof T]: … }` with `T` (`name`) at `xt`: `Some` with the result when `xt` is a
    /// union (the mapped type of each member, `null` / `undefined` kept) or a primitive (itself).
    fn distribute_mapped(
        &mut self,
        t: &ast::TypeExpr,
        m: &ast::MappedType,
        env: &TyEnv,
        name: &str,
        xt: TyId,
    ) -> Option<TyId> {
        let primitive = |cx: &Self, t: TyId| {
            matches!(
                cx.ty.kind(t),
                TyKind::Str | TyKind::Bool | TyKind::Int(_) | TyKind::Float(_) | TyKind::Literal(_)
            )
        };
        if primitive(self, xt) {
            return Some(xt);
        }
        let (core, nothing) = match *self.ty.kind(xt) {
            TyKind::Option(p, n) => (p, Some(n)),
            _ => (xt, None),
        };
        let members = self.union_members(core);
        if members.is_none() && nothing.is_none() {
            return None;
        }
        let mut out = vec![];
        for member in members.unwrap_or_else(|| vec![core]) {
            if primitive(self, member) {
                out.push(member);
                continue;
            }
            let mut inner = env.clone();
            inner.locals.push((name.to_string(), member));
            let r = self.resolve_mapped(t, m, &inner);
            if r == self.ty.error {
                return Some(r);
            }
            out.push(r);
        }
        Some(match (out.as_slice(), nothing) {
            ([one], None) => *one,
            _ => self.union_with(&out, nothing, t.span),
        })
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
                self.rejoin_booleans(&mut kept);
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

/// `s` as an array index (a canonical integer below 2^32 - 1), the keys JavaScript orders first.
fn array_index(s: &str) -> Option<u32> {
    let i: u32 = s.parse().ok()?;
    (i < u32::MAX && i.to_string() == s).then_some(i)
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
            K::Array(e) | K::KeyOf(e) | K::Construct(e) | K::TupleRest(e) | K::TupleOptional(e) => {
                go(e, out)
            }
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
            K::Callable { sigs, members } => {
                go(&crate::call_sigs::lower(sigs, members, t.span), out)
            }
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
