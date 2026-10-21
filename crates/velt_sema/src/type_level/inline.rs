//! Type-level forms written in a generic definition (not in an alias) that need its type
//! parameters (#350): `{ [K in keyof M]: Topic<M[K]> }` in a generic function's signature,
//! `T[K]`, `keyof T`, `Partial<T>`. Such a form is the deferred `TyOp::Inline` over the
//! definition's type parameters, evaluated again at every instantiation with the arguments
//! known, as a generic alias's body is (`Ctx::alias_body`). Two forms written alike in one
//! definition are one type, so `{} as { [K in keyof M]: V }` is the declared return type.

use rustc_hash::FxHashMap;

use velt_common::Span;
use velt_syntax::ast;

use crate::ctx::Ctx;
use crate::hir::{TyId, TyKind, TyOp};
use crate::resolve::TyEnv;

/// One form, as written.
pub(crate) struct Form {
    module: usize,
    params: Vec<String>,
    this_ty: Option<TyId>,
    ty: ast::TypeExpr,
    /// The form as shown in messages.
    pub(crate) shown: String,
    /// The requirement of a read `x[k]` (`Intrinsic::KeyRead`): `T[K]` where `K` must be one
    /// key, never a value's type.
    read: bool,
    /// The requirement of a write `x[k] = v` (`Intrinsic::KeyWrite`): as `read`, and the
    /// object type `T` is at each instantiation has a field assigned (`AdtDef::assigned`).
    write: bool,
}

/// The forms, and each one's id by where it is written and how.
#[derive(Default)]
pub(crate) struct Forms {
    pub(crate) forms: Vec<Form>,
    ids: FxHashMap<(usize, Vec<String>, String), u32>,
    /// The requirement of each read or write `x[k]`, by the types of `x` and `k` and where it
    /// is (`crate::inst_sites`).
    pub(crate) reads: FxHashMap<(TyId, TyId, Span), TyId>,
    /// The object types written through a key at some instantiation (`Form::write`).
    pub(crate) written: rustc_hash::FxHashSet<crate::hir::DefId>,
}

impl Ctx<'_> {
    /// A deferred type as written: `PropsAccessor<TProps>` (`names`: as for `display_in`).
    pub(crate) fn display_op(&self, o: TyOp, args: &[TyId], names: &[String]) -> String {
        if let TyOp::ErrSite(_) = o {
            return "never".to_string(); // As written: it throws nothing (`throws::vars`).
        }
        let args: Vec<String> = args.iter().map(|t| self.display_in(*t, names)).collect();
        let TyOp::Alias(a) = o else {
            if let TyOp::Inline(i) = o {
                return self.type_level.inline.forms[i as usize].shown.clone();
            }
            return args.join(crate::generic_spread::op_sep(o));
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

    /// Can a form written in `env` be deferred: in a generic definition, outside an alias and
    /// any form being evaluated?
    pub(crate) fn inline_deferrable(&self, env: &TyEnv) -> bool {
        self.type_level.deferring == 0
            && !self.checking_unused_aliases
            && !env.params.is_empty()
            && env.args.is_empty()
            && env.locals.is_empty()
            && env.unknowns.is_empty()
    }

    /// The form `t`, resolved by `resolve` in `env` (see [`Self::inline_deferrable`]); the
    /// deferred `TyOp::Inline` when it needs a type parameter of `env`.
    pub(crate) fn resolve_inline(
        &mut self,
        t: &ast::TypeExpr,
        env: &TyEnv,
        resolve: impl FnOnce(&mut Self) -> TyId,
    ) -> TyId {
        self.type_level.deferring += 1;
        let outer_stuck = std::mem::replace(&mut self.type_level.stuck, false);
        let reported = self.diags.len();
        let r = resolve(self);
        let stuck = std::mem::replace(&mut self.type_level.stuck, outer_stuck);
        self.type_level.deferring -= 1;
        if !stuck {
            return r;
        }
        // Evaluated again, and reported, once the arguments are known.
        self.diags.truncate(reported);
        self.inline_op(t, env, None)
    }

    /// The requirement that the read (or `write`) `x[k]` of the form `t` (`T[K]`) names one
    /// key at every instantiation (`Form::read`), written at `span`.
    pub(crate) fn key_read_requirement(
        &mut self,
        t: &ast::TypeExpr,
        env: &TyEnv,
        write: bool,
        span: Span,
    ) -> TyId {
        let op = self.inline_op(t, env, Some(write));
        self.type_level.op_sites.insert(op, span);
        op
    }

    /// Form `f` as written.
    pub(crate) fn inline_form_ast(&self, f: u32) -> ast::TypeExpr {
        self.type_level.inline.forms[f as usize].ty.clone()
    }

    /// The deferred form `t` of `env` (a read's requirement when `access` is, a write's when
    /// it is `Some(true)`).
    fn inline_op(&mut self, t: &ast::TypeExpr, env: &TyEnv, access: Option<bool>) -> TyId {
        // A read's requirement is its own (reported where the read is); other forms written
        // alike in one definition are one type.
        let (read, write) = (access.is_some(), access == Some(true));
        let site = access.map(|w| (w, t.span));
        let key = (
            env.module,
            env.params.clone(),
            format!("{site:?}{}", shape_key(t)),
        );
        let id = match self.type_level.inline.ids.get(&key) {
            Some(&id) => id,
            None => {
                let id = self.type_level.inline.forms.len() as u32;
                self.type_level.inline.forms.push(Form {
                    module: env.module,
                    params: env.params.clone(),
                    this_ty: env.this_ty,
                    ty: t.clone(),
                    shown: type_text(t),
                    read,
                    write,
                });
                self.type_level.inline.ids.insert(key, id);
                id
            }
        };
        // The parameters the form names; the others are `void`, so the form is known as soon
        // as the ones it needs are (`K extends keyof T` once `T` is inferred).
        let mut named = vec![];
        names_in(t, &mut named);
        // An alias declared in the body names them unseen (`AliasInfo::outer`).
        let all = named.contains(&"*")
            || (self.aliases.iter())
                .any(|a| !a.outer.is_empty() && named.contains(&a.decl.name.name.as_str()));
        let args = (env.params.iter().enumerate())
            .map(|(i, p)| match all || named.contains(&p.as_str()) {
                true => self.ty.param(i as u32),
                false => self.ty.unit,
            })
            .collect();
        let op = self.ty.intern(TyKind::Op(TyOp::Inline(id), args));
        self.type_level.op_sites.entry(op).or_insert(t.span);
        op
    }

    /// Form `i` at the known type arguments `args`, used at `span`.
    pub(crate) fn reduce_inline(&mut self, i: u32, args: Vec<TyId>, span: Span) -> TyId {
        let f = &self.type_level.inline.forms[i as usize];
        let ty = f.ty.clone();
        let (read, write, this_ty) = (f.read, f.write, f.this_ty);
        let mut env = TyEnv::new(f.module, &f.params);
        env.this_ty = this_ty.map(|t| self.subst(t, &args));
        env.args = args.clone();
        let outermost = self.type_level.alias_site.is_none();
        if outermost {
            self.type_level.alias_site = Some(span);
        }
        let mut body = self.resolve_type(&ty, &env);
        if read && !self.ty.has_error(body) {
            body = self.one_key(&ty, &env, body, span);
        }
        if let (true, ast::TypeExprKind::Indexed { object, .. }) = (write, &ty.kind) {
            let o = self.resolve_type(object, &env);
            let o = self.subst(o, &args);
            if let TyKind::Adt(d, _) = *self.ty.kind(o) {
                self.type_level.inline.written.insert(d);
            }
        }
        if outermost {
            self.type_level.alias_site = None;
        }
        let t = self.subst(body, &args);
        self.reduce_known_intersection(t)
    }

    /// `body`, the type of a read `x[k]` (form `form`, `T[K]`, at known arguments), when `K` is
    /// one key, or keys whose fields have one type (a switch on the key reads them); else
    /// `Error`, reported at `span`.
    fn one_key(&mut self, form: &ast::TypeExpr, env: &TyEnv, body: TyId, span: Span) -> TyId {
        let ast::TypeExprKind::Indexed { object, key } = &form.kind else {
            return body;
        };
        let k = self.resolve_type(key, env);
        let k = self.subst(k, &env.args);
        let members = self.union_members(k).unwrap_or_else(|| vec![k]);
        if members.len() == 1 {
            return body;
        }
        let o = self.resolve_type(object, env);
        let o = self.subst(o, &env.args);
        let fields = self.public_fields(o);
        let mut tys = vec![];
        for m in &members {
            let Some(crate::hir::LitValue::Str(name)) = self.lit_value(*m) else {
                return body;
            };
            match fields.iter().find(|f| f.name == name) {
                Some(f) if !f.optional => tys.push(self.canon(f.ty)),
                _ => tys.push(self.ty.error),
            }
        }
        if tys.iter().all(|&t| t == tys[0] && t != self.ty.error) {
            return body;
        }
        let shown = self.display(k);
        self.error(
            velt_common::Diagnostic::error(
                format!("this generic function reads or writes `x[k]` with the key type `{shown}` at this instantiation, whose fields have different types; that is not supported yet (#350)"),
                span,
            )
            .with_note("the keys' fields must have one type (or pass one key, a string literal)"),
        );
        self.ty.error
    }
}

/// The single names `t` mentions (type parameters among them, perhaps shadowed).
fn names_in<'t>(t: &'t ast::TypeExpr, out: &mut Vec<&'t str>) {
    let mut add = |x: &'t ast::TypeExpr| names_in(x, out);
    use ast::TypeExprKind as K;
    match &t.kind {
        K::Named { path, args } => {
            if let [one] = &path[..] {
                out.push(&one.name);
            }
            for a in args {
                names_in(a, out);
            }
        }
        K::Array(e) | K::KeyOf(e) | K::TupleRest(e) | K::TupleOptional(e) | K::Construct(e) => {
            add(e)
        }
        K::Tuple(ts) | K::Union(ts) | K::Intersection(ts) => ts.iter().for_each(add),
        K::Indexed { object, key } => {
            add(object);
            add(key);
        }
        K::Function {
            params,
            ret,
            throws,
            ..
        } => {
            params.iter().for_each(&mut add);
            add(ret);
            if let Some(t) = throws {
                add(t);
            }
        }
        K::GenericFunction { ty, .. } => add(ty),
        K::Object(fs) => fs.iter().for_each(|f| names_in(&f.ty, out)),
        K::IndexSig { fields, value, .. } => {
            fields.iter().for_each(|f| names_in(&f.ty, out));
            names_in(value, out);
        }
        K::Conditional(c) => {
            for x in [&c.check, &c.extends, &c.then, &c.else_] {
                names_in(x, out);
            }
        }
        K::Mapped(m) => {
            names_in(&m.source, out);
            names_in(&m.value, out);
            if let Some(n) = &m.name {
                names_in(n, out);
            }
        }
        K::TemplateLiteral { types, .. } => types.iter().for_each(add),
        K::Predicate { ty: Some(t), .. } => add(t),
        // `typeof x`, call signatures: every parameter, to be safe.
        K::Query(_) | K::Callable { .. } => out.push("*"),
        _ => {}
    }
}

/// `t` as written, without its positions: forms written alike have one key.
fn shape_key(t: &ast::TypeExpr) -> String {
    let full = format!("{:?}", t.kind);
    let mut out = String::with_capacity(full.len());
    let mut rest = full.as_str();
    while let Some(i) = rest.find("Span {") {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        match rest.find('}') {
            Some(j) => rest = &rest[j + 1..],
            None => break,
        }
    }
    out.push_str(rest);
    out
}

/// `t` written out for messages (`…` for what it doesn't spell).
pub(crate) fn type_text(t: &ast::TypeExpr) -> String {
    use ast::TypeExprKind as K;
    let list =
        |ts: &[ast::TypeExpr], sep: &str| ts.iter().map(type_text).collect::<Vec<_>>().join(sep);
    match &t.kind {
        K::Named { path, args } => {
            let name: Vec<&str> = path.iter().map(|i| i.name.as_str()).collect();
            match args.is_empty() {
                true => name.join("."),
                false => format!("{}<{}>", name.join("."), list(args, ", ")),
            }
        }
        K::Array(e) => format!("{}[]", type_text(e)),
        K::Union(ps) => list(ps, " | "),
        K::Intersection(ps) => list(ps, " & "),
        K::Indexed { object, key } => format!("{}[{}]", type_text(object), type_text(key)),
        K::KeyOf(e) => format!("keyof {}", type_text(e)),
        K::Literal(l) => match &l.lit {
            ast::Lit::Str(s) => format!("{s:?}"),
            _ => "…".into(),
        },
        K::Mapped(m) => format!(
            "{{ [{} in {}]: {} }}",
            m.param.name,
            type_text(&m.source),
            type_text(&m.value)
        ),
        K::Conditional(c) => format!(
            "{} extends {} ? {} : {}",
            type_text(&c.check),
            type_text(&c.extends),
            type_text(&c.then),
            type_text(&c.else_)
        ),
        K::Null => "null".into(),
        K::Undefined => "undefined".into(),
        _ => "…".into(),
    }
}
