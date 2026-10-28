//! Standard-library code checked on demand (#1658, docs/internals/design/lazy-std.md). The
//! prelude imports modules that most programs never use (`velt:number/parse`,
//! `velt:json/reader`, …), and checking all of their bodies cost every program, hello world
//! included, although lowering only builds what `main` reaches. So std definitions are
//! *deferred*: checked once checked code reaches them. [`reach`] runs after the other bodies:
//! it scans the checked HIR for references to deferred functions and constants (a call, a
//! function value, a read) and for the classes and structs it uses (any type in it), checks what
//! they name (all the methods of a used type), and repeats until nothing new is reached. What
//! nothing reaches stays unchecked: the program holds a stub for each (an empty body), listed in
//! `hir::Program::unchecked`, which lowering never builds (hir_encodings.md "Unchecked
//! definitions").
//!
//! Deferred, in a std module that no user module imports directly (nor uses as its JSX
//! runtime) and that has no top-level statements:
//! - free functions and static methods, except, in the prelude ([`HELPERS`]), the compiler's
//!   helpers that lowering looks up by name ([`is_helper`]);
//! - outside the prelude, module constants, and the methods and constructors of its classes and
//!   structs;
//! - nothing in the modules lowering looks into by name for other definitions ([`EAGER`]).
//!
//! A user module's code is always checked, and so is a std module it imports: `velt check`
//! reports everything in what a program imports, and bench/compile/all_std.vlt, which imports
//! every module, checks all of std. The editor's analysis checks everything.

use rustc_hash::{FxHashMap, FxHashSet};

use crate::ctx::Ctx;
use crate::defs::{BodyState, DefInfo, FnKind};
use crate::hir::{self, Callee, DefId, ExprKind as E, TyId, TyKind};

/// Std modules (path prefixes) checked whole: lowering looks up their definitions by name
/// (`velt_vir` `STABLE_SORT`, the proxy and typed-array glue, the epoch sections' `pin`,
/// `unpin` and `retire`) and calls them.
const EAGER: &[&str] = &[
    "std/sort",
    "std/proxy",
    "std/typed_array",
    "std/sync/epoch",
    "std/this_fn",
];

/// Std modules whose definitions lowering's JSON glue calls by name (`velt_vir` `json_std_fn`:
/// `std/json/reader::Reader.<method>`, `std/json/writer::<fn>`): all of a module's deferred
/// definitions are reached once checked code instantiates an intrinsic whose glue calls it
/// (`lazy_std_json.rs`). What they call in other modules is reached from their code as usual.
const JSON_GLUE: [&str; 2] = ["std/json/reader", "std/json/writer"];

/// The prelude's modules (path prefixes): lowering looks up the methods of their classes and
/// some of their functions by name ([`is_helper`]), so only their other free functions and
/// static methods are deferred.
const HELPERS: &[&str] = &["std/prelude/", "std/record", "std/collections"];

/// The prelude functions lowering looks up by name (`velt_vir` `prelude_def`, `LOOSE_EQ`,
/// `FLOAT_INDEX`, `TLA_MARK`, `track_caller`, …): every `"__…"` name in the code of lowering, the
/// optimizer and the backends (`tests::every_name_lowering_looks_up_is_a_helper`); classes too.
pub(crate) const HELPER_NAMES: &[&str] = &[
    "__ArrayMethod",
    "__ArrayView",
    "__ClassMarker",
    "__HasOwnProperty",
    "__HasOwnPropertyProxy",
    "__ThisFn",
    "__any",
    "__anyConcatPlain",
    "__anyConcatText",
    "__anyJoin",
    "__anyJoinSeen",
    "__anyLooseEq",
    "__anyPrimKeep",
    "__anyPrimText",
    "__anyPrimTry",
    "__anyString",
    "__anyText",
    "__anyTextUnsupported",
    "__anyToPrimitive",
    "__anyToPrimitivePlain",
    "__apply",
    "__arrayCall",
    "__arrayViewFlush",
    "__arrayViewFlushAt",
    "__arrayViewFlushFrom",
    "__arrayViewLen",
    "__arrayViewNew",
    "__arrayViewSort",
    "__arrayViewSync",
    "__arrayViewSyncAt",
    "__arrayViewSyncLen",
    "__bind",
    "__classObject",
    "__debug_line",
    "__decoded",
    "__deepEquals",
    "__defineGetter__",
    "__defineSetter__",
    "__delete",
    "__entryAt",
    "__floatIndex",
    "__get",
    "__has",
    "__hasGet",
    "__inAny",
    "__isView",
    "__json",
    "__keyAt",
    "__keyNames",
    "__keys",
    "__liveAt",
    "__lookupGetter__",
    "__lookupSetter__",
    "__mulToInt32",
    "__placeholder",
    "__plain",
    "__positions",
    "__proto__",
    "__seen",
    "__set",
    "__target",
    "__topLevelAwaitAt",
    "__uncaughtHeader",
    "__valueAt",
];

/// Is `name` (a definition's short name) one of the compiler's helpers: in [`HELPER_NAMES`], or
/// a built-in method of a value held as `any` (`__any<Type>_<method>`, `velt_vir`
/// `builtin_methods`)?
pub(crate) fn is_helper(name: &str) -> bool {
    HELPER_NAMES.contains(&name)
        || name
            .strip_prefix("__any")
            .is_some_and(|r| r.starts_with(|c: char| c.is_ascii_uppercase()) && r.contains('_'))
}

/// How a module's definitions are checked.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Eager,
    /// Free functions, static methods, constants and its types' methods on demand.
    Lazy,
    /// Free functions and static methods on demand, except the compiler's helpers.
    Helpers,
}

/// What is checked on demand in one check.
pub(crate) struct Lazy {
    /// Off for the editor's analysis (`ide`), which checks everything.
    pub on: bool,
    /// Per module: how it is checked (empty while off).
    modules: Vec<Mode>,
    /// The definitions [`reach`] has scanned.
    scanned: FxHashSet<DefId>,
    /// The types met in scanned code.
    types: FxHashSet<TyId>,
    /// The classes and structs met in scanned code (directly or as a part of a type).
    used: FxHashSet<DefId>,
    /// Per class or struct: its deferred methods and constructors.
    methods: FxHashMap<DefId, Vec<DefId>>,
    /// When the [`JSON_GLUE`] modules are reached.
    json: json::JsonReach,
}

impl Lazy {
    /// On, unless `VELT_EAGER_STD` is set (to compare); the editor's analysis turns it off.
    pub(crate) fn new() -> Lazy {
        Lazy {
            on: std::env::var_os("VELT_EAGER_STD").is_none(),
            modules: vec![],
            scanned: FxHashSet::default(),
            types: FxHashSet::default(),
            used: FxHashSet::default(),
            methods: FxHashMap::default(),
            json: json::JsonReach::default(),
        }
    }
}

/// Decide which modules are lazy (once per check, before the bodies are checked).
pub(crate) fn plan(cx: &mut Ctx) {
    if !cx.lazy.on {
        return;
    }
    let mods = cx.modules;
    let mut direct: FxHashSet<&str> = FxHashSet::default();
    for m in mods.iter().filter(|m| !m.is_std) {
        direct.extend(m.imports.iter().map(|(_, p)| p.as_str()));
        direct.extend(m.jsx_runtime.as_deref());
    }
    cx.lazy.modules = (mods.iter().enumerate())
        .map(|(i, m)| {
            let lazy = m.is_std
                && i != cx.root
                && !direct.contains(m.path.as_str())
                && !EAGER.iter().any(|p| m.path.starts_with(p))
                && crate::module_scripts::script_fns(m).is_empty();
            match lazy {
                false => Mode::Eager,
                true if HELPERS.iter().any(|p| m.path.starts_with(p)) => Mode::Helpers,
                true => Mode::Lazy,
            }
        })
        .collect();
    for i in 0..cx.info.len() {
        let d = DefId(i as u32);
        if let (DefInfo::Fn(f), true) = (&cx.info[i], deferred(cx, d)) {
            if let Some(k) = JSON_GLUE.iter().position(|m| *m == mods[f.module].path) {
                cx.lazy.json.defs[k].push(d);
            }
            if let (Some(o), FnKind::Method | FnKind::Ctor) = (f.owner, f.kind) {
                cx.lazy.methods.entry(o).or_default().push(d);
            }
        }
    }
}

/// Is `d` a function or module constant checked only once checked code reaches it?
pub(crate) fn deferred(cx: &Ctx, d: DefId) -> bool {
    if cx.lazy.modules.is_empty() {
        return false;
    }
    match &cx.info[d.0 as usize] {
        DefInfo::Fn(f) if f.source.is_none() => false,
        DefInfo::Fn(f) => match f.kind {
            FnKind::Free | FnKind::Static => match cx.lazy.modules[f.module] {
                Mode::Eager => false,
                Mode::Lazy => true,
                Mode::Helpers => !f.name.rsplit(['.', ':']).next().is_some_and(is_helper),
            },
            // A type's methods, where the type is declared (an `extend` of a prelude type
            // keeps its methods checked).
            FnKind::Method | FnKind::Ctor => {
                f.owner.is_some_and(|o| match &cx.info[o.0 as usize] {
                    DefInfo::Adt(a) => cx.lazy.modules[a.module] == Mode::Lazy,
                    _ => false,
                })
            }
            _ => false,
        },
        DefInfo::Global(g) => {
            g.src.owner.is_none()
                && !g.mutable
                && !g.stored
                && cx.lazy.modules[g.module] == Mode::Lazy
        }
        _ => false,
    }
}

/// Check the deferred definitions that checked code reaches, and what their code reaches, until
/// nothing new is reached.
pub(crate) fn reach(cx: &mut Ctx) {
    if cx.lazy.modules.is_empty() {
        return;
    }
    // Defaults of fields and parameters may refer to them too: scanned once, first.
    let mut sc = Scan::default();
    for info in cx.info.iter_mut() {
        match info {
            DefInfo::Fn(f) => {
                for e in f.params.iter_mut().filter_map(|p| p.default.as_mut()) {
                    scan_expr(e, &mut sc);
                }
            }
            DefInfo::Adt(a) => {
                for e in a.fields.iter_mut().filter_map(|f| f.default.as_mut()) {
                    scan_expr(e, &mut sc);
                }
                for e in a.redefined.iter_mut().filter_map(|r| r.init.as_mut()) {
                    scan_expr(e, &mut sc);
                }
            }
            _ => {}
        }
    }
    loop {
        for i in 0..cx.info.len() {
            let d = DefId(i as u32);
            let done = match &cx.info[i] {
                DefInfo::Fn(_) => cx.defs[i].is_some(),
                DefInfo::Global(g) => g.state == BodyState::Done,
                _ => false,
            };
            if !done || !cx.lazy.scanned.insert(d) {
                continue;
            }
            sc.cur = Some(d);
            match (&mut cx.defs[i], &mut cx.info[i]) {
                (Some(hir::Def::Fn(f)), _) => {
                    sc.types.extend(f.body.locals.iter().map(|l| l.ty));
                    sc.types.push(f.ret);
                    crate::visit::exprs_mut(&mut f.body.block, &mut |e| refs_of(e, &mut sc));
                }
                (_, DefInfo::Global(g)) => {
                    sc.types.push(g.ty);
                    if let Some(e) = &mut g.init {
                        scan_expr(e, &mut sc);
                    }
                }
                _ => {}
            }
        }
        sc.cur = None;
        used_types(cx, std::mem::take(&mut sc.types), &mut sc.found);
        json::settle(cx, &mut sc);
        let mut checked = false;
        for d in std::mem::take(&mut sc.found) {
            if !deferred(cx, d) {
                continue;
            }
            match &cx.info[d.0 as usize] {
                DefInfo::Fn(f) if f.state == BodyState::Unchecked => {
                    super::ensure_body(cx, d);
                    checked = true;
                }
                DefInfo::Global(g) if g.state == BodyState::Unchecked => {
                    super::ensure_global(cx, d);
                    checked = true;
                }
                _ => {}
            }
        }
        if !checked {
            break;
        }
    }
}

/// Record the types `todo` and their parts as met; each class or struct met for the first time
/// adds its deferred methods (and those of its base classes, and its fields' types) to `found`.
fn used_types(cx: &mut Ctx, mut todo: Vec<TyId>, found: &mut Vec<DefId>) {
    while let Some(t) = todo.pop() {
        if !cx.lazy.types.insert(t) {
            continue;
        }
        let kind = cx.ty.kind(t).clone();
        todo.extend(crate::types::children(&kind));
        let TyKind::Adt(d, _) = kind else { continue };
        if !cx.lazy.used.insert(d) {
            continue;
        }
        if let Some(a) = cx.adt(d) {
            todo.extend(a.fields.iter().map(|f| f.ty));
            todo.extend(a.base);
        }
        found.extend(cx.lazy.methods.get(&d).into_iter().flatten().copied());
    }
}

/// The deferred definitions nothing reached, each given a stub (an empty function, or a
/// constant of `void` value) so that every definition has one; for `hir::Program::unchecked`.
pub(crate) fn stub_unreached(cx: &mut Ctx) -> FxHashSet<DefId> {
    let mut out = FxHashSet::default();
    for i in 0..cx.info.len() {
        let d = DefId(i as u32);
        if cx.defs[i].is_some() || !deferred(cx, d) {
            continue;
        }
        let def = match &cx.info[i] {
            DefInfo::Fn(f) if f.state == BodyState::Unchecked => {
                // `this` first, as `body::driver::declare_params` declares them.
                let this = f
                    .this
                    .as_ref()
                    .map(|t| ("this".to_string(), t.ty, t.mode, f.name_span));
                let params: Vec<_> = (this.into_iter())
                    .chain(
                        f.params
                            .iter()
                            .map(|p| (p.name.clone(), p.ty, p.mode, p.span)),
                    )
                    .collect();
                let locals = (params.iter())
                    .map(|(name, ty, _, span)| hir::LocalDef {
                        name: name.clone(),
                        ty: *ty,
                        mutable: false,
                        boxed: false,
                        span: *span,
                    })
                    .collect();
                let params = (params.iter().enumerate())
                    .map(|(k, (_, ty, mode, _))| hir::Param {
                        local: hir::LocalId(k as u32),
                        ty: *ty,
                        mode: *mode,
                    })
                    .collect();
                hir::Def::Fn(hir::FnDef {
                    name: f.name.clone(),
                    generics: f.generics.len() as u32,
                    params,
                    ret: f.ret,
                    is_async: f.is_async || f.is_async_gen,
                    is_generator: f.is_generator,
                    self_ty: (f.this.as_ref())
                        .filter(|_| !f.has_this_param())
                        .map(|t| t.ty),
                    captures: vec![],
                    shares_captures: false,
                    body: hir::Body {
                        locals,
                        block: hir::Block {
                            stmts: vec![],
                            value: None,
                            span: f.span,
                        },
                    },
                    throws: None,
                    span: f.span,
                })
            }
            DefInfo::Global(g) if g.state == BodyState::Unchecked => {
                // `finalize::build_defs` makes its definition (a constant's is built there).
                let unit = cx.ty.unit;
                let span = g.span;
                if let DefInfo::Global(g) = &mut cx.info[i] {
                    g.ty = unit;
                    g.init = Some(hir::Expr {
                        kind: E::Lit(hir::Lit::Unit),
                        ty: unit,
                        span,
                    });
                }
                out.insert(d);
                continue;
            }
            _ => continue,
        };
        cx.defs[i] = Some(def);
        out.insert(d);
    }
    out
}

/// What scanned code refers to.
#[derive(Default)]
struct Scan {
    /// Definitions it names.
    found: Vec<DefId>,
    /// Types of its expressions and locals.
    types: Vec<TyId>,
    /// The function being scanned (none for defaults and constants).
    cur: Option<DefId>,
    /// The JSON intrinsics it uses: the [`JSON_GLUE`] module, the function they are in, and
    /// their types (`json::settle`).
    json: Vec<(usize, Option<DefId>, Vec<TyId>)>,
    /// Its calls (and function values) with type arguments: caller, callee, type arguments.
    calls: Vec<(Option<DefId>, DefId, Vec<TyId>)>,
    /// Whether it calls a generic method through an interface or dynamically.
    generic_dispatch: bool,
}

/// The definitions and types `e` refers to, into `sc` (`visit` walks mutable HIR; nothing is
/// changed).
fn scan_expr(e: &mut hir::Expr, sc: &mut Scan) {
    crate::visit::exprs_in(e, &mut |e| refs_of(e, sc));
}

fn refs_of(e: &hir::Expr, sc: &mut Scan) {
    sc.types.push(e.ty);
    match &e.kind {
        E::Call {
            callee: Callee::Def(d, targs),
            ..
        }
        | E::FnRef(d, targs) => {
            sc.found.push(*d);
            if !targs.is_empty() {
                sc.calls.push((sc.cur, *d, targs.clone()));
            }
        }
        E::Global(d) => sc.found.push(*d),
        E::Call {
            callee: Callee::Intrinsic(i),
            args,
        } => {
            if let Some(k) = json::json_glue(i) {
                let tys = (args.iter().map(|a| a.ty)).chain([e.ty]).collect();
                sc.json.push((k, sc.cur, tys));
            }
        }
        E::Call {
            callee:
                Callee::Dyn {
                    method_type_args, ..
                }
                | Callee::ParamMethod {
                    method_type_args, ..
                },
            ..
        } if !method_type_args.is_empty() => sc.generic_dispatch = true,
        _ => {}
    }
}

#[path = "lazy_std_json.rs"]
mod json;

#[cfg(test)]
#[path = "lazy_std_tests.rs"]
mod tests;
