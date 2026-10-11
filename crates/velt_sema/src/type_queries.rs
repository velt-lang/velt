//! TypeScript's type operators on values and keys (docs/reference/types.md "Type queries and
//! `keyof`"):
//!
//! - `typeof x` in a type: the type of a local, module constant or function, and `typeof a.b`
//!   the type of a field of one. A function's type includes its inferred result, so `typeof f`
//!   in a free function's signature waits until collection is finished (`Ctx::sig_fill`).
//! - `keyof T` on an object type: the union of its keys as string literal types.
//! - `ReturnType<F>` and `Parameters<F>` on a function type.
//!
//! Anything Velt cannot represent exactly (a generic function's type, a class's constructor
//! type, `keyof` of a class) is an error saying it is not supported yet, never an
//! approximation.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::ctx::{Ctx, Item};
use crate::defs::{DefInfo, FnSource, RetSource};
use crate::hir::{AdtKind, DefId, TyId, TyKind};
use crate::resolve::TyEnv;

/// Free functions' signatures with a type query that needs a body checked (`ReturnType<typeof
/// f>` of a function whose result is inferred) wait until collection is finished
/// (`Ctx::bodies_ok`, `collect::sigs::fill_deferred`).
#[derive(Default)]
pub(crate) struct SigQueries<'m> {
    /// A free function's signature is being resolved: a type query that needs a body defers it.
    pub sig_fill: bool,
    /// Set when the signature being resolved was deferred (`sig_fill`).
    pub sig_deferred: bool,
    /// The deferred signatures; a type query fills one on demand (`collect::sigs::fill_pending`).
    pub deferred_sigs: std::collections::BTreeMap<DefId, (&'m ast::FnSig, usize)>,
    /// Those being filled now, each with the name of the parameter (or function) whose type is
    /// being resolved: a query reaching one of them is a cycle (TS2502).
    pub filling_sigs: Vec<(DefId, String)>,
}

impl Ctx<'_> {
    /// `typeof x` / `typeof ns.f` / `typeof x.field`.
    pub(crate) fn resolve_query(
        &mut self,
        t: &ast::TypeExpr,
        path: &[ast::Ident],
        env: &TyEnv,
    ) -> TyId {
        // An empty path stands for a query the parser already reported.
        let Some(head) = path.first() else {
            return self.ty.error;
        };
        if head.name == "this" {
            self.err("`typeof this` is not supported yet (#936)", t.span);
            return self.ty.error;
        }
        let local = self
            .query_locals
            .iter()
            .rev()
            .find(|(n, _)| *n == head.name)
            .map(|&(_, ty)| ty);
        let (mut ty, rest) = match local {
            Some(ty) => (ty, &path[1..]),
            None => {
                let ns =
                    path.len() >= 2 && self.scopes[env.module].namespaces.contains_key(&head.name);
                let (item, rest) = match ns {
                    true => (
                        self.lookup_path_at(env.module, &path[..2], t.span),
                        &path[2..],
                    ),
                    false => (
                        self.lookup_item_at(env.module, &head.name, head.span),
                        &path[1..],
                    ),
                };
                let named = path[..path.len() - rest.len()]
                    .iter()
                    .map(|i| i.name.as_str())
                    .collect::<Vec<_>>()
                    .join(".");
                let ty = match item {
                    Some(Item::Def(d)) => self.query_def(d, &named, t.span),
                    Some(Item::Alias(_)) => {
                        self.only_a_type(&named, t.span);
                        return self.ty.error;
                    }
                    None => {
                        self.err(format!("cannot find `{named}` in this scope"), t.span);
                        return self.ty.error;
                    }
                };
                (ty, rest)
            }
        };
        for seg in rest {
            if ty == self.ty.error {
                return ty;
            }
            ty = self.query_field(ty, seg, t.span);
        }
        if let TyKind::Closure(_) = self.ty.kind(ty) {
            self.err(
                "`typeof` of a local function value is not supported yet (#936)",
                t.span,
            );
            return self.ty.error;
        }
        ty
    }

    fn only_a_type(&mut self, name: &str, span: Span) {
        self.err(
            format!("`{name}` is a type, not a value: `typeof` needs a value"),
            span,
        );
    }

    /// The type of the value item `d` (named `name`).
    fn query_def(&mut self, d: DefId, name: &str, span: Span) -> TyId {
        match &self.info[d.0 as usize] {
            DefInfo::Fn(_) => self.query_fn(d, name, span),
            DefInfo::Global(g) => {
                let (module, ann, init) = (g.module, g.src.ann, g.src.init);
                if let Some(ann) = ann {
                    return self.resolve_type(ann, &TyEnv::new(module, &[]));
                }
                // `const s = "a"` is the literal type `"a"`, as in TypeScript.
                if let Some(lit) = init.and_then(literal_init) {
                    // `const N = 4` is the literal type `4` of `number` in TypeScript; Velt's
                    // number literal types divide as integers and take no arithmetic yet
                    // (#462), so it is `number` until they do.
                    if let ast::Lit::Int { suffix: None, .. }
                    | ast::Lit::Float { suffix: None, .. } = lit.lit
                    {
                        return self.ty.f64;
                    }
                    return match self.lit_value_of(&lit, span) {
                        Some(v) => self.lit_type(v),
                        None => self.ty.error,
                    };
                }
                if !self.bodies_ok {
                    return self.query_needs_body(name, "constant", span);
                }
                crate::body::ensure_global(self, d);
                self.global(d).map_or(self.ty.error, |g| g.ty)
            }
            DefInfo::Adt(_) | DefInfo::Enum(_) => {
                self.error(
                    Diagnostic::error(
                        format!("`typeof {name}` (the type of `{name}` itself, not of its values) is not supported yet (#936)"),
                        span,
                    )
                    .with_note(format!("the type of its instances is `{name}`")),
                );
                self.ty.error
            }
            DefInfo::Iface(_) => {
                self.only_a_type(name, span);
                self.ty.error
            }
        }
    }

    /// `typeof f` for function `f`: its function type, with the result its body gives.
    fn query_fn(&mut self, d: DefId, name: &str, span: Span) -> TyId {
        if self.bodies_ok {
            // `function a(x: ReturnType<typeof b>)` before `b`: fill `b`'s signature first,
            // unless it is the one being resolved (TS2502).
            if let Some(i) = self.sigq.filling_sigs.iter().position(|f| f.0 == d) {
                let whose = self.sigq.filling_sigs.last().map_or("", |f| f.1.as_str());
                let whose = if whose.is_empty() { name } else { whose };
                let cycle: Vec<String> = self.sigq.filling_sigs[i..]
                    .iter()
                    .map(|f| format!("`{}`", self.fn_info(f.0).name))
                    .collect();
                let note = match &cycle[..] {
                    [_] => format!("the signature of `{name}` needs its own type"),
                    [rest @ .., last] => format!(
                        "the signatures of {} and {last} need each other's types",
                        rest.join(", ")
                    ),
                    [] => unreachable!("ICE: an empty cycle of signatures"),
                };
                self.error(
                    Diagnostic::error(format!("`{whose}` is referenced in its own type"), span)
                        .with_note(note),
                );
                return self.ty.error;
            }
            crate::collect::sigs::fill_pending(self, d);
        }
        let f = self.fn_info(d);
        if f.generics.len() > 0 {
            self.error(
                Diagnostic::error(
                    format!("`typeof {name}` of a generic function is not supported yet (#936)"),
                    span,
                )
                .with_note("Velt's function types have no type parameters"),
            );
            return self.ty.error;
        }
        if f.is_async || f.is_generator {
            let what = if f.is_generator {
                "a generator"
            } else {
                "an async"
            };
            self.err(
                format!("`typeof {name}` of {what} function is not supported yet (#936)"),
                span,
            );
            return self.ty.error;
        }
        let ast_params = match f.source {
            Some(FnSource::Decl(decl)) => &decl.sig.params[..],
            Some(FnSource::Default(sig, _)) => &sig.params[..],
            None => &crate::collect::sigs::extern_sig(self, d, f.module).params[..],
        };
        let special = ast_params.iter().find_map(|p| match () {
            _ if p.rest => Some("a rest parameter"),
            _ if p.optional => Some("an optional parameter"),
            _ if p.default.is_some() => Some("a parameter with a default"),
            _ => None,
        });
        if let Some(what) = special {
            self.err(
                format!("`typeof {name}` of a function with {what} is not supported yet (#936)"),
                span,
            );
            return self.ty.error;
        }
        if !self.bodies_ok {
            return self.query_needs_body(name, "function", span);
        }
        let f = self.fn_info(d);
        let params: Vec<TyId> = f.params.iter().map(|p| p.ty).collect();
        let ret = match f.ret_source {
            RetSource::Known => f.ret,
            _ => crate::body::returns::ret_of(self, d, span),
        };
        let never = self.ty.never;
        let throws = crate::throws::throws_now(self, d, &[]).unwrap_or(never);
        self.ty.intern(TyKind::FnPtr {
            params,
            ret,
            throws,
        })
    }

    /// A query whose answer needs a body checked, before collection is finished: a free
    /// function's signature waits for the end of collection (`Ctx::sig_fill`); anywhere else it is
    /// not supported yet.
    fn query_needs_body(&mut self, name: &str, what: &str, span: Span) -> TyId {
        if self.sigq.sig_fill {
            self.sigq.sig_deferred = true;
        } else {
            self.error(
                Diagnostic::error(
                    format!("`typeof {name}` of a {what} is not supported in this position yet (#936)"),
                    span,
                )
                .with_note("it is supported in function bodies and in the signatures of functions, not in classes and interfaces (fields and methods)"),
            );
        }
        self.ty.error
    }

    /// The type of field `seg` of a value of type `ty` (`typeof config.port`).
    fn query_field(&mut self, ty: TyId, seg: &ast::Ident, span: Span) -> TyId {
        let shown = self.display(ty);
        let fields = match self.ty.kind(ty).clone() {
            TyKind::Adt(d, _) if self.adt(d).is_some_and(|a| a.kind != AdtKind::Struct) => {
                self.object_fields("typeof", ty, None, span)
            }
            _ => None,
        };
        let Some(fields) = fields else {
            self.err(
                format!("`typeof` of a member of `{shown}` is not supported yet (#936)"),
                seg.span,
            );
            return self.ty.error;
        };
        match fields.into_iter().find(|f| f.name == seg.name) {
            Some(f) if f.optional => self.ty.option(f.ty),
            Some(f) => f.ty,
            None => {
                self.err(
                    format!("no field `{}` on type `{shown}`", seg.name),
                    seg.span,
                );
                self.ty.error
            }
        }
    }

    /// `keyof T`: the union of an object type's keys (`"a" | "b"`), or the keys of a record
    /// with literal keys.
    pub(crate) fn resolve_keyof(
        &mut self,
        t: &ast::TypeExpr,
        inner: &ast::TypeExpr,
        env: &TyEnv,
    ) -> TyId {
        let ty = self.resolve_type(inner, env);
        if ty == self.ty.error {
            return ty;
        }
        let shown = self.display(ty);
        match self.ty.kind(ty).clone() {
            TyKind::Adt(d, args) if Some(d) == self.prelude_adt("Record") => {
                if args[0] != self.ty.str_ {
                    return args[0];
                }
                self.err(
                    format!("`keyof {shown}` (`string | number` in TypeScript) is not supported yet (#936)"),
                    t.span,
                );
                self.ty.error
            }
            TyKind::Adt(d, _) if self.adt(d).is_some_and(|a| a.kind == AdtKind::Anon) => {
                let Some(fields) = self.object_fields("keyof", ty, None, t.span) else {
                    return self.ty.error;
                };
                let keys: Vec<TyId> = fields
                    .into_iter()
                    .map(|f| self.lit_type(crate::hir::LitValue::Str(f.name)))
                    .collect();
                match keys.len() {
                    0 => self.ty.never,
                    1 => keys[0],
                    _ => self.union_of(&keys, false, t.span),
                }
            }
            _ => {
                self.err(
                    format!("`keyof {shown}` is not supported yet (#936): `keyof` works on object types"),
                    t.span,
                );
                self.ty.error
            }
        }
    }

    /// `ReturnType<F>` / `Parameters<F>` on a function type `F`.
    pub(crate) fn resolve_fn_operator(
        &mut self,
        t: &ast::TypeExpr,
        name: &str,
        args: &[ast::TypeExpr],
        env: &TyEnv,
    ) -> TyId {
        let tys: Vec<TyId> = args.iter().map(|a| self.resolve_type(a, env)).collect();
        let [f] = tys[..] else {
            self.err(
                format!(
                    "type `{name}` takes 1 type argument(s) but {} were supplied",
                    tys.len()
                ),
                t.span,
            );
            return self.ty.error;
        };
        if f == self.ty.error {
            return f;
        }
        match self.ty.kind(f).clone() {
            TyKind::FnPtr { params, ret, .. } => match name {
                "ReturnType" => ret,
                // A parameter already reported (a cycle): no tuple with an error inside.
                _ if params.contains(&self.ty.error) => self.ty.error,
                _ => self.ty.intern(TyKind::Tuple(params)),
            },
            TyKind::Param(_) => {
                self.err(
                    format!("`{name}` of a type parameter is not supported yet (#209)"),
                    t.span,
                );
                self.ty.error
            }
            _ => {
                let shown = self.display(f);
                self.err(
                    format!("`{name}` needs a function type; found `{shown}`"),
                    t.span,
                );
                self.ty.error
            }
        }
    }
}

/// The literal a constant's initializer is (`"a"`, `4`, `-1`, `true`), as a literal type.
fn literal_init(e: &ast::Expr) -> Option<ast::SignedLit> {
    match &e.kind {
        ast::ExprKind::Lit(
            lit @ (ast::Lit::Str(_)
            | ast::Lit::Int { .. }
            | ast::Lit::Float { .. }
            | ast::Lit::Bool(_)),
        ) => Some(ast::SignedLit {
            lit: lit.clone(),
            negative: false,
        }),
        ast::ExprKind::Unary {
            op: ast::UnaryOp::Neg,
            expr,
        } => match &expr.kind {
            ast::ExprKind::Lit(lit @ (ast::Lit::Int { .. } | ast::Lit::Float { .. })) => {
                Some(ast::SignedLit {
                    lit: lit.clone(),
                    negative: true,
                })
            }
            _ => None,
        },
        ast::ExprKind::Paren(inner) => literal_init(inner),
        _ => None,
    }
}
