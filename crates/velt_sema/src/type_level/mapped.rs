//! Mapped types over literal keys (`{ readonly [K in S as N]?: V }`): their fields, in the order
//! TypeScript gives them (`Ctx::written_keys`), and the distribution of a homomorphic one over a
//! union.

use velt_syntax::ast;

use super::*;

impl Ctx<'_> {
    /// The keys a mapped type's `in` clause `src` (in `module`) writes, in the order written: a
    /// union of string or number literals, through a non-generic alias naming one, the first
    /// argument of `Exclude`/`Extract` (what is left keeps that order), or `keyof X` (`X`'s
    /// fields, resolved in `env` when there is one). `None` when the clause has no written order.
    fn written_keys(
        &mut self,
        src: &ast::TypeExpr,
        module: usize,
        env: Option<&TyEnv>,
        depth: usize,
    ) -> Option<Vec<String>> {
        use ast::TypeExprKind as T;
        if depth > 16 {
            return None;
        }
        match &src.kind {
            T::Literal(ast::SignedLit {
                lit: ast::Lit::Str(s),
                ..
            }) => Some(vec![s.clone()]),
            T::Literal(_) => {
                // A number key is spelled as the resolved literal is (`literal_keys`).
                let t = self.resolve_type(src, env?);
                Some(self.literal_keys(t)?.into_iter().map(|(k, _)| k).collect())
            }
            T::Union(ms) => {
                let mut out: Vec<String> = vec![];
                for m in ms {
                    for k in self.written_keys(m, module, env, depth + 1)? {
                        if !out.contains(&k) {
                            out.push(k);
                        }
                    }
                }
                Some(out)
            }
            T::KeyOf(x) => {
                let xt = self.resolve_type(x, env?);
                let fields = self.relatable_fields(xt)?;
                Some(fields.into_iter().map(|f| f.name).collect())
            }
            T::Named { path, args } => {
                let last = path.last()?.name.as_str();
                if path.len() == 1 && matches!(last, "Exclude" | "Extract") && !args.is_empty() {
                    return self.written_keys(&args[0], module, env, depth + 1);
                }
                let item = self.lookup_type_path_at(module, path, src.span)?;
                let crate::ctx::Item::Alias(a) = item else {
                    return None;
                };
                let alias = &self.aliases[a as usize];
                if !args.is_empty() || !alias.decl.generics.is_empty() || !alias.outer.is_empty() {
                    return None;
                }
                let (body, at) = (&alias.decl.ty, alias.module);
                // The alias's body is in its own module, with no environment of this use's.
                self.written_keys(body, at, None, depth + 1)
            }
            _ => None,
        }
    }

    /// `{ readonly [K in S as N]?: V }`.
    pub(crate) fn resolve_mapped(
        &mut self,
        t: &ast::TypeExpr,
        m: &ast::MappedType,
        env: &TyEnv,
    ) -> TyId {
        // `[K in keyof X]` maps `X`'s fields, keeping their `?` and `readonly` (homomorphic).
        let source_fields = match &m.source.kind {
            ast::TypeExprKind::KeyOf(x) => {
                let xt = self.resolve_type(x, env);
                let Some(xt) = self.known_type(xt, env, "a mapped type", x.span) else {
                    return self.ty.error;
                };
                // Over `any`, `any`, as the utility types are (`PropsAccessor<any>`).
                if self.dynamic(xt) == Some(crate::any::Dynamic::Any) {
                    return xt;
                }
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
        if let Some(r) = self.mapped_index_signature(m, env, src) {
            return r;
        }
        let Some(keys) = self.literal_keys(src) else {
            let shown = self.display(src);
            self.err(
                format!("a mapped type over `{shown}` (not a union of literal keys) is not supported yet; write `Record<{shown}, V>`"),
                m.source.span,
            );
            return self.ty.error;
        };
        let mut out: Vec<ShapeField> = vec![];
        // The fields keep the order TypeScript gives them: `X`'s for `[K in keyof X]`, else the
        // keys' as written (`[K in "b" | "a"]`, an alias of such a union, `Exclude<keyof X, …>`),
        // not the order of the key union's members, which is canonical (`Ctx::canon_key`).
        let order: Option<Vec<String>> = match &source_fields {
            Some(fs) => Some(fs.iter().map(|f| f.name.clone()).collect()),
            None => self.written_keys(&m.source, env.module, Some(env), 0),
        };
        // Per field of `out`, where its key is in `order`.
        let mut ranks: Vec<usize> = vec![];
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
                ranks.push(
                    (order.as_ref())
                        .and_then(|o| o.iter().position(|k| *k == key))
                        .unwrap_or(usize::MAX),
                );
                out.push(ShapeField {
                    name,
                    ty: v,
                    readonly,
                    optional,
                });
            }
        }
        let mut ranked: Vec<(usize, ShapeField)> = ranks.into_iter().zip(out).collect();
        ranked.sort_by_key(|(rank, _)| *rank);
        let mut out: Vec<ShapeField> = ranked.into_iter().map(|(_, f)| f).collect();
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
}
