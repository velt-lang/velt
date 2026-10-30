//! Generated functions (docs/internals/design/source-generators.md): `declare generated("<spec>")
//! function f<T>(…): R;` is a generic function whose body a source generator writes for each
//! concrete instance the program reaches.
//!
//! Sema's part, per check:
//! - a generated function's own body is a placeholder that is never built (`check_fn` gives it
//!   a `panic`); callers are checked against its signature;
//! - [`demand::compute`] carries calls of generated functions through generic code to their
//!   concrete instances, as the JSON requirement is carried (`crate::json`);
//! - the instances the driver already generated ([`GenInput`]: items it appended to the declaring
//!   modules) are bound to their generated definitions ([`bind::bind`]), which go to lowering as
//!   `hir::Program::generated`;
//! - when an instance has no generated definition, the check returns [`Demand`] (the requests,
//!   with the type model each generator reads) instead of a program, and the driver checks again
//!   with the generated code added.

pub(crate) mod bind;
pub(crate) mod demand;
pub mod model;
pub mod spell;

use velt_common::{FileId, Span};

use crate::ctx::Ctx;
use crate::defs::{DefInfo, FnSource};
use crate::hir::DefId;

/// The name a generated module uses for a type it cannot import: `__qualified<"<module path>",
/// "<name>", …type arguments>` (written by the driver from [`spell`]), resolved only in
/// generated files.
pub const QUALIFIED: &str = "__qualified";

/// What the driver generated so far: items it appended to the declaring modules, in files listed
/// in `files`.
#[derive(Clone, Debug, Default)]
pub struct GenInput {
    pub instances: Vec<GenInstance>,
    /// The generated source files (their items are generated code: `__qualified` resolves there).
    pub files: Vec<FileId>,
}

/// One generated instance: in module `module` (the declaring module's path), the generated
/// function `function` at the types the type aliases `type_args` name, whose body is the
/// function item `body` of the same module.
#[derive(Clone, Debug)]
pub struct GenInstance {
    pub module: String,
    pub function: String,
    pub type_args: Vec<String>,
    pub body: String,
}

/// What a check that found instances without generated code returns instead of a program.
#[derive(Clone, Debug, Default)]
pub struct Demand {
    /// Every generated function the program declares (a generator may ask for instances of any
    /// function of its module).
    pub functions: Vec<GenFunction>,
    pub requests: Vec<Request>,
    /// The types the requests reach (one model shared by all of them).
    pub model: model::Model,
}

/// A generated function's declaration, its types written as generated code spells them with
/// type parameter `i` as [`spell::param_placeholder`]`(i)`.
#[derive(Clone, Debug)]
pub struct GenFunction {
    /// The declaring module's path and the function's name in it.
    pub module: String,
    pub function: String,
    /// The generator module's specifier as written, and its span.
    pub generator: String,
    pub generator_span: Span,
    pub params: Vec<(String, String)>,
    /// The result type (`Promise<…>` for an async function).
    pub ret: String,
    pub is_async: bool,
    /// The declared `throws` type.
    pub throws: Option<String>,
}

/// A request for the body of one instance of a generated function.
#[derive(Clone, Debug)]
pub struct Request {
    /// Index into [`Demand::functions`].
    pub function: usize,
    /// The call that first reached the instance.
    pub at: Span,
    /// The type arguments, as ids into the model.
    pub type_args: Vec<u32>,
}

impl Ctx<'_> {
    /// The generator spec of a generated function.
    pub(crate) fn generator_of(&self, d: DefId) -> Option<&velt_syntax::ast::GeneratorSpec> {
        match self.info.get(d.0 as usize)? {
            DefInfo::Fn(f) => match f.source {
                Some(FnSource::Decl(decl)) => decl.sig.generator.as_ref(),
                _ => None,
            },
            _ => None,
        }
    }

    /// Does the program declare a generated function? (No cost for programs without one.)
    pub(crate) fn has_generated_fns(&self) -> bool {
        self.modules.iter().any(|m| {
            m.ast.items.iter().any(|i| {
                matches!(&i.kind, velt_syntax::ast::ItemKind::Function(f) if f.sig.generator.is_some())
            })
        })
    }

    /// Is `span` in a generated source file?
    pub(crate) fn in_generated_file(&self, span: Span) -> bool {
        self.gen.is_some_and(|g| g.files.contains(&span.file))
    }
}

impl crate::body::FnCx<'_, '_> {
    /// The own body of generated function `f`: a `panic` lowering never builds (each instance
    /// is built from its generated definition), so callers see only the signature (a missing
    /// result type is `void`, as for every declaration without a body).
    pub(crate) fn generated_placeholder(
        &mut self,
        f: &crate::defs::FnInfo,
        out: &mut Vec<crate::hir::Stmt>,
    ) {
        let span = f.name_span;
        let msg = self.str_lit("ICE: the own body of a generated function ran", span);
        let never = self.cx.ty.never;
        let p = self.intrinsic(crate::hir::Intrinsic::Panic, vec![msg], never, span);
        out.push(crate::hir::Stmt {
            kind: crate::hir::StmtKind::Expr(p),
            span,
        });
    }
}

/// The generated definition of every instance of a generated function the checked program
/// reaches, or, when some have none yet, the requests for them.
pub(crate) fn instances(cx: &mut Ctx) -> Result<bind::Bound, Demand> {
    if !cx.has_generated_fns() {
        return Ok(bind::Bound::default());
    }
    let bound = std::mem::take(&mut cx.gen_bound);
    check_throws(cx, &bound);
    let found = demand::compute(cx);
    let missing: Vec<demand::Instance> = (found.into_iter())
        .filter(|(d, a, _)| !bound.contains_key(&(*d, a.clone())))
        .collect();
    if missing.is_empty() {
        return Ok(bound);
    }
    let mut model = model::Builder::new();
    let (functions, index) = functions(cx);
    let mut requests = vec![];
    for (g, targs, at) in missing {
        let Some(&function) = index.get(&g) else {
            continue;
        };
        let mut unnamed = None;
        let mut type_args = vec![];
        for t in &targs {
            let id = model.add(cx, *t);
            if model.model.types[id as usize].spelling.is_empty() {
                unnamed = Some(*t);
            }
            type_args.push(id);
        }
        if let Some(t) = unnamed {
            let tn = cx.display(t);
            cx.error(velt_common::Diagnostic::error(
                format!("generated code cannot name the type `{tn}` (a type declared inside a function?)"),
                at,
            ));
            continue;
        }
        requests.push(Request {
            function,
            at,
            type_args,
        });
    }
    Err(Demand {
        functions,
        requests,
        model: model.model,
    })
}

impl Ctx<'_> {
    /// `__qualified<"<module path>", "<name>", …args>` in generated code: the type `name`
    /// declared (exported or not) in that module, with those type arguments.
    pub(crate) fn resolve_qualified(
        &mut self,
        t: &velt_syntax::ast::TypeExpr,
        args: &[velt_syntax::ast::TypeExpr],
        env: &crate::resolve::TyEnv,
    ) -> crate::hir::TyId {
        use velt_syntax::ast::{Lit, TypeExprKind};
        let text = |a: Option<&velt_syntax::ast::TypeExpr>| match a.map(|a| &a.kind) {
            Some(TypeExprKind::Literal(l)) => match &l.lit {
                Lit::Str(s) => Some(s.clone()),
                _ => None,
            },
            _ => None,
        };
        let (Some(module), Some(name)) = (text(args.first()), text(args.get(1))) else {
            self.err(
                format!("`{QUALIFIED}` takes a module path and a name as string literal types"),
                t.span,
            );
            return self.ty.error;
        };
        let item = (self.modules.iter().position(|m| m.path == module))
            .and_then(|m| self.scopes[m].items.get(&name).copied());
        let Some(item) = item else {
            self.err(
                format!("module `{module}` declares no type `{name}`"),
                t.span,
            );
            return self.ty.error;
        };
        self.item_type(item, &name, &args[2..], t, env)
    }
}

/// A generated body throws what its function declares and nothing else: callers were checked
/// against the declaration, and call it with the declaration's error ABI.
fn check_throws(cx: &mut Ctx, bound: &bind::Bound) {
    for ((g, _), body) in bound {
        if cx.fn_info(*g).declared_throws.is_some() {
            continue;
        }
        let Some(t) = cx.fn_info(*body).throws.filter(|t| !cx.ty.is_bottom(*t)) else {
            continue;
        };
        let (name, tn) = (cx.fn_info(*g).name.clone(), cx.display(t));
        let span = cx.fn_info(*body).name_span;
        cx.error(velt_common::Diagnostic::error(
            format!("the code generated for `{name}` throws `{tn}`, which its declaration does not list"),
            span,
        ).with_note(format!("the declaration needs `throws {tn}` for its generated code to throw")));
    }
}

/// Every generated function of the program, with its index.
fn functions(cx: &mut Ctx) -> (Vec<GenFunction>, rustc_hash::FxHashMap<DefId, usize>) {
    let mut out = vec![];
    let mut index = rustc_hash::FxHashMap::default();
    for d in (0..cx.info.len() as u32).map(DefId) {
        let Some(spec) = cx.generator_of(d).cloned() else {
            continue;
        };
        let f = cx.fn_info(d).clone();
        let mut params = vec![];
        for p in &f.params {
            let s = spell::spell_template(cx, p.ty).unwrap_or_default();
            params.push((p.name.clone(), s));
        }
        let ret = spell::spell_template(cx, f.ret).unwrap_or_default();
        let throws = match f.declared_throws.as_ref().and_then(|t| t.ty) {
            Some(t) => spell::spell_template(cx, t),
            None => None,
        };
        index.insert(d, out.len());
        out.push(GenFunction {
            module: cx.modules[f.module].path.clone(),
            function: f.name.rsplit("::").next().unwrap_or(&f.name).to_string(),
            generator: spec.spec,
            generator_span: spec.span,
            params,
            ret,
            is_async: f.is_async,
            throws,
        });
    }
    (out, index)
}
