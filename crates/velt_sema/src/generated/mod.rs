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

pub(crate) mod access;
pub mod arm;
pub(crate) mod bind;
pub(crate) mod demand;
pub mod handles;
pub mod model;
pub(crate) mod phase;
pub mod spell;

use velt_common::{FileId, Span};

use crate::ctx::Ctx;
use crate::defs::{DefInfo, FnSource};
use crate::hir::{AdtKind, DefId};

/// The name a generated module uses for a type it cannot import: `__qualified<"<module path>",
/// "<name>", …type arguments>` (written by the driver from [`spell`]), resolved only in
/// generated files.
pub const QUALIFIED: &str = "__qualified";

/// `__union<[<members>], …type arguments>` in generated code: a generic union at those type
/// arguments, its members naming its type parameters `__T0`, `__T1`, … (written by the driver
/// from [`spell`]: written out, `T | Mod<T>` at `T = unknown` would be `unknown`).
pub const UNION: &str = "__union";

/// What the driver generated so far: generated modules it added to the program
/// (docs/internals/design/source-generators.md §7), and the instances in them.
#[derive(Clone, Debug, Default)]
pub struct GenInput {
    pub instances: Vec<GenInstance>,
    pub modules: Vec<GenModule>,
    /// The generated source files (their items are generated code: `__qualified` and
    /// `__construct` resolve there).
    pub files: Vec<FileId>,
}

impl GenInput {
    pub fn extend(&mut self, other: GenInput) {
        self.instances.extend(other.instances);
        self.modules.extend(other.modules);
        self.files.extend(other.files);
    }
}

/// A generated module: never a std module, checked in the scope of the module declaring the
/// generated functions it implements (that module's imports and top-level declarations,
/// exported or not, as if it were appended to it), with the access of the types it was
/// generated for.
#[derive(Clone, Debug)]
pub struct GenModule {
    /// Its path among the program's modules.
    pub path: String,
    /// The declaring module's path.
    pub declaring: String,
    /// The qualified names of the declared types of its requests' models: its code may use their
    /// `private` members and build them with `__construct`.
    pub access: Vec<String>,
}

/// One generated instance: of the generated function `function` declared in module `module`,
/// at the types the handles `type_args` name ([`handles`]), whose body is the function item
/// `body` of the generated module `body_module`.
#[derive(Clone, Debug)]
pub struct GenInstance {
    pub module: String,
    pub function: String,
    pub type_args: Vec<String>,
    pub body: String,
    pub body_module: String,
    /// The key of the demanded instance the code was generated for ([`Request::fingerprint`]:
    /// the [`model::Model::fingerprint`] of its types, with the function's
    /// [`GenFunction::signature`]): its module is checked once that instance is demanded, so
    /// code offered from an earlier build (docs/internals/design/source-generators.md §9) is
    /// used only while the types it was generated for are still the same. Empty for an
    /// instance a generator asked for itself (`Output.instance`), which is checked with the
    /// module of the instance that asked.
    pub fingerprint: String,
}

/// What the driver generated for a [`Demand`] during a check: the generated modules, parsed,
/// and what they hold.
#[derive(Default)]
pub struct GenBatch {
    pub input: GenInput,
    pub sources: Vec<crate::SourceModule>,
}

/// How the driver generates during a check: given the requests of the instances without code,
/// it runs their generators and returns the code (`None` when it could not, with the errors
/// reported on its side). Called on sema's thread.
pub type GenerateFn<'a> = dyn Fn(&Demand) -> Option<GenBatch> + Sync + 'a;

/// The most generated modules one check adds itself (`phase`); more start a check again.
const GENERATED_SLOTS: usize = 64;

/// The driver's generator during one check, and the modules it made, which outlive every
/// `Ctx` of the check (sema may check again from scratch, with them, `crate::check_timed`).
pub struct Generator<'a> {
    pub run: &'a GenerateFn<'a>,
    slots: Vec<std::sync::OnceLock<crate::SourceModule>>,
    pub(crate) state: std::cell::RefCell<GenState>,
}

/// What a [`Generator`] added during the check.
#[derive(Default)]
pub(crate) struct GenState {
    /// Slots filled, each with the index of its module's declaring module.
    pub filled: Vec<usize>,
    /// What the added modules hold.
    pub input: GenInput,
    /// The generator failed (its errors are the driver's), or made code a check cannot add to
    /// itself (`restart`): the check stops and the driver checks again.
    pub failed: bool,
    pub restart: bool,
}

impl<'a> Generator<'a> {
    pub fn new(run: &'a GenerateFn<'a>) -> Self {
        Generator {
            run,
            slots: (0..GENERATED_SLOTS)
                .map(|_| std::sync::OnceLock::new())
                .collect(),
            state: Default::default(),
        }
    }

    /// Keep module `m`, generated for module `declaring`, for the rest of the check: `None`
    /// when no slot is left.
    pub(crate) fn keep(
        &self,
        m: crate::SourceModule,
        declaring: usize,
    ) -> Option<&crate::SourceModule> {
        let i = self.state.borrow().filled.len();
        let slot = self.slots.get(i)?;
        slot.set(m).ok()?;
        self.state.borrow_mut().filled.push(declaring);
        slot.get()
    }

    /// The modules kept so far, in order.
    pub(crate) fn kept(&self) -> impl Iterator<Item = &crate::SourceModule> {
        self.slots.iter().map_while(|s| s.get())
    }
}

/// What a check that may use generated functions is given: the code generated so far, and the
/// driver's generator when it generates during the check.
#[derive(Clone, Copy)]
pub struct GenArgs<'a> {
    pub input: &'a GenInput,
    /// The generated modules of `input` that are not among the program's modules: added to the
    /// check as they are, without the source rewrites (each passes [`phase::needs_rewrite`]).
    pub sources: &'a [crate::SourceModule],
    pub generate: Option<&'a GenerateFn<'a>>,
}

pub use phase::needs_rewrite;

/// What a check that found instances without generated code returns instead of a program.
#[derive(Clone, Debug, Default)]
pub struct Demand {
    /// Every generated function the program declares (a generator may ask for instances of any
    /// function of its module).
    pub functions: Vec<GenFunction>,
    pub requests: Vec<Request>,
    /// The types the requests reach (one model shared by all of them).
    pub model: model::Model,
    /// Code offered from an earlier build was stale (generated for types that changed since).
    pub stale: bool,
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

impl GenFunction {
    /// The signature as generated code spells it, for fingerprints.
    pub fn signature(&self) -> String {
        let params: Vec<String> = (self.params.iter())
            .map(|(n, t)| format!("{n}: {t}"))
            .collect();
        format!(
            "{}{}({}): {} throws {}",
            if self.is_async { "async " } else { "" },
            self.function,
            params.join(", "),
            self.ret,
            self.throws.as_deref().unwrap_or("-")
        )
    }
}

/// A check's generated code (`Ctx::gen`): what the driver generated so far, with what this check
/// generated (`input`), the driver's generator when it generates during the check, and the
/// binding of the instances.
#[derive(Default)]
pub(crate) struct Gen<'m> {
    pub input: Option<std::rc::Rc<GenInput>>,
    pub generate: Option<&'m Generator<'m>>,
    pub binding: Binding,
}

/// What binding the instances to their generated definitions found ([`phase::run`]).
#[derive(Default)]
pub(crate) struct Binding {
    /// The instances bound, once the bodies are checked.
    pub bound: bind::Bound,
    /// The types handles in generated code name ([`handles`]).
    pub handles: handles::Handles,
    /// What computing the demand walked so far ([`demand::compute`]).
    pub walked: demand::Walked,
    /// Some instance offered from an earlier build was not bound: its types changed.
    pub stale: bool,
    /// Top-level non-generic aliases by their type (module, name, without its nothing): how
    /// generated code names a type it cannot write out ([`spell`]).
    pub alias_homes: rustc_hash::FxHashMap<crate::hir::TyId, (usize, String, bool)>,
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
    /// The instance's key ([`phase::keys`]): what the handles of the types its code names
    /// start with, and how the next check finds it again.
    pub fingerprint: String,
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

    /// Is module `m` a generated module?
    pub(crate) fn is_generated_module(&self, m: usize) -> bool {
        (self.gen.input.as_ref()).is_some_and(|g| g.files.contains(&self.modules[m].file))
    }

    /// Is `span` in a generated source file?
    pub(crate) fn in_generated_file(&self, span: Span) -> bool {
        (self.gen.input.as_ref()).is_some_and(|g| g.files.contains(&span.file))
    }
}

impl crate::body::FnCx<'_, '_> {
    /// The own body of `f` (declared by `src`) when it is a generated function: a `panic`
    /// lowering never builds (each instance is built from its generated definition), so callers
    /// see only the signature (a missing result type is `void`, as for every declaration
    /// without a body).
    pub(crate) fn generated_placeholder(
        &mut self,
        src: crate::defs::FnSource,
        f: &crate::defs::FnInfo,
        out: &mut Vec<crate::hir::Stmt>,
    ) {
        if !matches!(src, crate::defs::FnSource::Decl(d) if d.sig.generator.is_some()) {
            return;
        }
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
    let bound = std::mem::take(&mut cx.gen.binding.bound);
    check_throws(cx, &bound);
    let found = demand::compute(cx);
    let is_missing = |(d, a, _): &demand::Instance| !bound.contains_key(&(*d, a.clone()));
    // A check that runs no generators (sema's own API, its tests and tools) does not need
    // std's generated code (`JSON.stringify`): it is only built by the driver, which runs them.
    let std_decl = |cx: &Ctx, d: DefId| {
        cx.fn_info(d).module < cx.modules.len() && cx.modules[cx.fn_info(d).module].is_std
    };
    let missing: Vec<&demand::Instance> = found.iter().filter(|i| is_missing(i)).collect();
    if cx.gen.input.is_none()
        && cx.gen.generate.is_none()
        && missing.iter().all(|(d, _, _)| std_decl(cx, *d))
    {
        return Ok(bound);
    }
    match demand_of(cx, &found, &bound) {
        Some(d) => Err(d),
        None => Ok(bound),
    }
}

/// The requests for the instances of `found` that `bound` has no code for, with the model they
/// read; `None` when there are none.
pub(crate) fn demand_of(
    cx: &mut Ctx,
    found: &[demand::Instance],
    bound: &bind::Bound,
) -> Option<Demand> {
    let is_missing = |(d, a, _): &demand::Instance| !bound.contains_key(&(*d, a.clone()));
    if !found.iter().any(is_missing) {
        return None;
    }
    // Every demanded instance's key (a key's rank counts the bound ones too), then a model of
    // what the missing ones reach.
    let keys = phase::keys(cx, &mut model::Builder::new(), found);
    let mut model = model::Builder::new();
    let (functions, index) = functions(cx);
    let mut requests = vec![];
    let mut roots = vec![];
    for (inst, (key, _, _)) in found.iter().zip(keys) {
        let (g, targs, at) = inst;
        let Some(&function) = index.get(g).filter(|_| is_missing(inst)) else {
            continue;
        };
        let type_args: Vec<u32> = targs.iter().map(|t| model.add(cx, *t)).collect();
        roots.push((key.clone(), type_args.clone()));
        requests.push(Request {
            function,
            at: *at,
            type_args,
            fingerprint: key,
        });
    }
    let mut model = model.model;
    handles::assign(&mut model, &roots);
    Some(Demand {
        functions,
        requests,
        model,
        stale: cx.gen.binding.stale,
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

impl Ctx<'_> {
    /// `__union<[members], …args>` in generated code ([`UNION`]).
    pub(crate) fn resolve_generated_union(
        &mut self,
        t: &velt_syntax::ast::TypeExpr,
        args: &[velt_syntax::ast::TypeExpr],
        env: &crate::resolve::TyEnv,
    ) -> crate::hir::TyId {
        use velt_syntax::ast::TypeExprKind;
        let members = match args.first().map(|a| &a.kind) {
            Some(TypeExprKind::Tuple(ms)) => ms.clone(),
            _ => {
                self.err(
                    format!("`{UNION}` takes the members as a tuple type, then the type arguments"),
                    t.span,
                );
                return self.ty.error;
            }
        };
        let rest = &args[1..];
        let names: Vec<String> = (0..rest.len())
            .map(|i| format!("{}{i}", spell::UNION_PARAM))
            .collect();
        let mut inner = crate::resolve::TyEnv::new(env.module, &names);
        inner.this_ty = env.this_ty;
        let ms: Vec<crate::hir::TyId> = members
            .iter()
            .map(|m| self.resolve_type(m, &inner))
            .collect();
        let generic = self.union_of(&ms, false, t.span);
        let targs: Vec<crate::hir::TyId> = rest.iter().map(|a| self.resolve_type(a, env)).collect();
        self.subst(generic, &targs)
    }

    /// The class `__qualified<"<module path>", "<name>">` or a handle (`__type<…>`) names in
    /// generated code (without its type arguments, no diagnostics): what `instanceof` tests and
    /// narrows to there.
    pub(crate) fn qualified_class(&self, t: &velt_syntax::ast::TypeExpr) -> Option<DefId> {
        use velt_syntax::ast::{Lit, TypeExprKind};
        let TypeExprKind::Named { path, args } = &t.kind else {
            return None;
        };
        if path.len() != 1 || !self.in_generated_file(t.span) {
            return None;
        }
        if path[0].name == handles::TYPE {
            let (Some(TypeExprKind::Literal(fp)), Some(TypeExprKind::Literal(i))) =
                (args.first().map(|a| &a.kind), args.get(1).map(|a| &a.kind))
            else {
                return None;
            };
            let (Lit::Str(fp), Lit::Int { value, .. }) = (&fp.lit, &i.lit) else {
                return None;
            };
            let ty = self
                .gen
                .binding
                .handles
                .get(fp, u32::try_from(*value).ok()?)?;
            return match self.ty.kind(ty) {
                crate::hir::TyKind::Adt(d, _)
                    if self.adt(*d).is_some_and(|a| a.kind == AdtKind::Class) =>
                {
                    Some(*d)
                }
                _ => None,
            };
        }
        if path[0].name != QUALIFIED {
            return None;
        }
        let text = |a: &velt_syntax::ast::TypeExpr| match &a.kind {
            TypeExprKind::Literal(l) => match &l.lit {
                Lit::Str(s) => Some(s.clone()),
                _ => None,
            },
            _ => None,
        };
        let [module, name] = args.as_slice() else {
            return None;
        };
        let (module, name) = (text(module)?, text(name)?);
        let m = self.modules.iter().position(|m| m.path == module)?;
        match self.scopes[m].items.get(&name)? {
            crate::ctx::Item::Def(d) if self.adt(*d).is_some_and(|a| a.kind == AdtKind::Class) => {
                Some(*d)
            }
            _ => None,
        }
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
        let Some(f) = gen_function(cx, d) else {
            continue;
        };
        index.insert(d, out.len());
        out.push(f);
    }
    (out, index)
}

/// Generated function `d`'s declaration as the driver sees it.
pub(crate) fn gen_function(cx: &mut Ctx, d: DefId) -> Option<GenFunction> {
    let spec = cx.generator_of(d).cloned()?;
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
    Some(GenFunction {
        module: cx.modules[f.module].path.clone(),
        function: f.name.rsplit("::").next().unwrap_or(&f.name).to_string(),
        generator: spec.spec,
        generator_span: spec.span,
        params,
        ret,
        is_async: f.is_async,
        throws,
    })
}
