//! Standard-library code checked on demand (#1658, docs/internals/design/lazy-std.md). The
//! prelude imports modules that most programs never use (`velt/lang/number/parse`,
//! `velt/encoding/json/reader`, …), and checking all of their bodies cost every program, hello world
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
//! - the methods of its `extend` blocks on number types, `bool`, `string` and `velt/simd`
//!   vectors, the prelude's included ([`primitive_extension`]): reached by a call or a function value, or through an
//!   interface implementation that holds one ([`impl_methods`]);
//! - nothing in the modules lowering looks into by name for other definitions ([`EAGER`]);
//! - the hooks lowering calls to convert an `any` to a record ([`RECORD_HOOKS`]), although
//!   lowering calls them: reached by a conversion that may need them ([`record_hooks_needed`]);
//! - the hooks whose table entry lists the constructs whose lowering calls them (velt_common
//!   `hook_demand.rs`: the integer hooks): reached once *live* code has one ([`live`]), which
//!   [`reach`] works out alongside what it checks; `hir::Program::hooks_demanded` tells lowering.
//!
//! A user module's code is always checked, and so is a std module it imports: `velt check`
//! reports everything in what a program imports, except the prelude's unreached free functions,
//! statics and extension methods. All of std is checked with laziness off (`VELT_CHECK_ALL_STD=1`),
//! as veltc's `all_of_std_checks_without_diagnostics` does for bench/compile/all_std.velt. The
//! editor's analysis checks everything.

use rustc_hash::{FxHashMap, FxHashSet};

use crate::ctx::Ctx;
use crate::defs::{BodyState, DefInfo, FnKind};
use crate::hir::{self, Callee, DefId, ExprKind as E, TyId, TyKind};

mod live;
mod number_text;
mod record_conversions;

use record_conversions::{holds_string_record, note_user_conversions};

pub(crate) use live::{
    assume_constructs, demanded as hooks_demanded, finish as finish_live, live_defs,
    thrown_needs_format,
};

/// Std modules (path prefixes) checked whole: lowering looks up their definitions by name
/// (`velt_vir` `STABLE_SORT`, the proxy and typed-array glue, the epoch sections' `pin`,
/// `unpin` and `retire`, `this_fn`'s glue, the `HostRef` hooks of host function calls) and calls
/// them.
const EAGER: &[&str] = &[
    velt_common::std_module!(lang_sort),
    velt_common::std_module!(lang_proxy),
    velt_common::std_module!(lang_typed_array),
    velt_common::std_module!(sync_epoch),
    velt_common::std_module!(lang_this_fn),
    velt_common::std_module!(host),
];

/// Prelude classes whose methods are checked once the type is used, as a class outside the
/// prelude's are: lowering looks up none of their methods by name except where it also has a
/// value of the type (`JsonValue[JSON_RUNTIME_HANDLE]` prints one; generated decoders call
/// [`LAZY_PRELUDE_STATICS`]), and their code reaches large modules most programs never load (`JsonValue`'s the tape,
/// velt/encoding/json/tape). `TupleElements`: a program that mentions `.map` loads it
/// (std/prelude/global/tuple_elements.velt), and only a tuple method call uses it.
const LAZY_PRELUDE_TYPES: &[&str] = &[
    velt_common::std_item!(prelude_json_value, "JsonValue"),
    velt_common::std_item!(lang_hooks_tuple_elements, "TupleElements"),
];

/// Static methods of [`LAZY_PRELUDE_TYPES`] checked with the type's methods, once the type is
/// used: `JsonValue[JSON_FROM_READER]`, which the decoder generated for a type holding a
/// `JsonValue` calls (it parses the value into a tape, so a decoder of other types checks none
/// of the tape).
const LAZY_PRELUDE_STATICS: &[&str] = &[concat!(
    velt_common::std_item!(prelude_json_value, "JsonValue."),
    velt_common::std_key!(json_from_reader)
)];

/// The prelude's modules (path prefixes): lowering looks up the methods of their classes and
/// some of their functions by name ([`is_helper`]), so only their other free functions and
/// static methods are deferred.
const HELPERS: &[&str] = &[
    velt_common::std_dir!(prelude),
    velt_common::std_dir!(lang_hooks),
    velt_common::std_module!(lang_record),
    velt_common::std_module!(collections),
    // The hooks lowering calls for C functions' pointers and strings.
    velt_common::std_dir!(ffi),
];

/// The prelude methods (and the few other names) lowering looks up by name: every `"__…"` name
/// in the code of lowering, the optimizer and the backends
/// (`tests::every_name_lowering_looks_up_is_a_helper`). The compiler's hooks lowering calls are
/// marked `lowered` in velt_common's `std_hooks.rs` instead.
pub(crate) const HELPER_NAMES: &[&str] = &[
    "__any",
    velt_common::std_key!(this_fn_apply),
    velt_common::std_key!(proxy_array_call),
    velt_common::std_key!(this_fn_bind),
    "__debug_line",
    velt_common::std_key!(record_decoded),
    "__defineGetter__",
    "__defineSetter__",
    velt_common::std_key!(record_delete),
    velt_common::std_key!(map_entry_at),
    velt_common::std_key!(record_get),
    velt_common::std_key!(record_has),
    velt_common::std_key!(proxy_has_get),
    velt_common::std_key!(record_in_any),
    velt_common::std_key!(proxy_is_view),
    velt_common::std_key!(proxy_json),
    velt_common::std_key!(map_key_at),
    velt_common::std_key!(record_key_names),
    velt_common::std_key!(proxy_keys),
    velt_common::std_key!(map_live_at),
    "__lookupGetter__",
    "__lookupSetter__",
    "__placeholder",
    velt_common::std_key!(proxy_plain),
    velt_common::std_key!(record_positions),
    "__proto__",
    velt_common::std_key!(record_set),
    velt_common::std_key!(proxy_target),
    velt_common::std_key!(proxy_get),
    velt_common::std_key!(proxy_set),
    velt_common::std_key!(proxy_has),
    velt_common::std_key!(proxy_delete),
    velt_common::std_key!(map_value_at),
];

/// Is the definition `qualified` (its full name) one of the compiler's helpers: a hook lowering
/// calls (`velt_common::std_hooks`, `lowered`), a name in [`HELPER_NAMES`], or a built-in method
/// of a value held as `any` (`any<Type>_<method>`, `velt_vir` `builtin_methods`)?
pub(crate) fn is_helper(qualified: &str) -> bool {
    let name = qualified.rsplit(['.', ':']).next().unwrap_or(qualified);
    (velt_common::std_hooks::is_lowered(qualified)
        && velt_common::std_hooks::find(qualified).is_some_and(|h| {
            !velt_common::std_hooks::ON_DEMAND_FLOAT.contains(&h.name) && h.constructs.is_empty()
        }))
        || HELPER_NAMES.contains(&name)
        || name
            .strip_prefix("any")
            .is_some_and(|r| r.starts_with(|c: char| c.is_ascii_uppercase()) && r.contains('_'))
}

/// The hooks that convert an `any` holding an object to a `Record<string, V>` (`velt_vir`
/// `any_to_record.rs`). Lowering calls them from the conversion of an `any` to a type holding
/// such a record, which no checked code names, but the element read through `any` an array
/// needs makes checking them cost a program that mentions `Record` 4% (#2052): they are checked
/// once [`record_hooks_needed`]. Lowering leaves a conversion whose hooks are unchecked the
/// `TypeError` it was without them.
pub(crate) const RECORD_HOOKS: &[&str] = &[
    velt_common::std_hook!(any_to_record_any),
    velt_common::std_hook!(any_to_record),
];

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
    /// Off for the editor's analysis (`ide`), which checks everything, unless it has a load
    /// callback: then `check_all`.
    pub on: bool,
    /// Check every body (the editor's analysis), and work out what is reached as if they were
    /// deferred, for one demand set with `velt check` (`std_load`): [`deferred`] is false, while
    /// [`defers`] tells what [`reach`] would check.
    pub check_all: bool,
    /// With `check_all`: the definitions [`reach`] would have checked.
    reached: FxHashSet<DefId>,
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
    /// Per class: the classes that extend it directly.
    subclasses: FxHashMap<DefId, Vec<DefId>>,
    /// The definitions below this one had their defaults scanned ([`reach`]).
    defaults_scanned: usize,
    /// Per definition when [`plan`] ran: whether it is a function [`deferred`] (`None`: not a
    /// function, decided when asked). What decides it for a function (its module, kind and
    /// owner) does not change during a check, and the passes ask about every definition.
    fn_deferred: Vec<Option<bool>>,
    /// Has checked code of a non-std module a type holding a record with string keys in it?
    user_record: bool,
    /// Does checked code of a non-std module convert a value of a type parameter to or from an
    /// `any` (`<T>(x: any): T`), which may be a record at run time?
    generic_any: bool,
    /// Does checked code convert an `any` to a type holding a record with string keys (an
    /// `AnyTo`), or a non-std module put a value whose members, elements or values hold one
    /// into an `any` (where a member written through the `any` converts to it)?
    record_conversion: bool,
    /// Have [`RECORD_HOOKS`] been reached?
    record_hooks: bool,
    /// The program's code converts a value that may hold a float to text: the float hooks are
    /// reached (`velt_common::std_hooks::ON_DEMAND_FLOAT`).
    float_text: bool,
    /// The live definitions, and the hooks their constructs demand ([`live`]).
    live: live::Live,
}

impl Lazy {
    /// The passes after the bodies' check have run: no std module can be added any more.
    pub(crate) fn finishing(&self) -> bool {
        self.live.finishing
    }

    /// On, unless `VELT_CHECK_ALL_STD` is set; the editor's analysis turns it off.
    pub(crate) fn new() -> Lazy {
        Lazy {
            on: std::env::var_os("VELT_CHECK_ALL_STD").is_none(),
            check_all: false,
            reached: FxHashSet::default(),
            modules: vec![],
            scanned: FxHashSet::default(),
            types: FxHashSet::default(),
            used: FxHashSet::default(),
            methods: FxHashMap::default(),
            subclasses: FxHashMap::default(),
            defaults_scanned: 0,
            fn_deferred: vec![],
            user_record: false,
            generic_any: false,
            record_conversion: false,
            record_hooks: false,
            float_text: false,
            live: live::Live::new(),
        }
    }

    /// A generated module added during the check (`Ctx::add_module`): checked eagerly, as any
    /// non-std module.
    pub(crate) fn add_module(&mut self) {
        if !self.modules.is_empty() {
            self.modules.push(Mode::Eager);
        }
    }
}

/// Decide which modules are lazy (once per check, before the bodies are checked).
pub(crate) fn plan(cx: &mut Ctx) {
    if !cx.lazy.on {
        return;
    }
    let mods = cx.modules.clone();
    let mut direct: FxHashSet<&str> = FxHashSet::default();
    // A generated module sees what its declaring module imports without importing it: it makes
    // no std module checked whole (the declaring module, a user's, already does for its own).
    let generated = |m: &crate::SourceModule| {
        cx.gen
            .input
            .as_ref()
            .is_some_and(|g| g.files.contains(&m.file))
    };
    for m in mods.iter().filter(|m| !m.is_std && !generated(m)) {
        direct.extend(m.imports.iter().map(|(_, p)| p.as_str()));
        direct.extend(m.jsx_runtime.as_deref());
    }
    cx.lazy.modules = (mods.iter().enumerate())
        .map(|(i, m)| mode_of(m, i != cx.root && !direct.contains(m.path.as_str())))
        .collect();
    plan_defs(cx, 0);
}

/// How module `m` is checked; `lazy`: the program does not ask for it whole.
fn mode_of(m: &crate::SourceModule, lazy: bool) -> Mode {
    let lazy = lazy
        && m.is_std
        && !EAGER.iter().any(|p| m.path.starts_with(p))
        && crate::module_scripts::script_fns(m).is_empty();
    match lazy {
        false => Mode::Eager,
        true if HELPERS.iter().any(|p| m.path.starts_with(p)) => Mode::Helpers,
        true => Mode::Lazy,
    }
}

/// [`plan`] for std modules `ms`, added during the check (`crate::std_load`), whose definitions
/// start at `from`. No user module imports them (it would have loaded them).
pub(crate) fn plan_added(cx: &mut Ctx, ms: &[usize], from: u32) {
    if cx.lazy.modules.is_empty() {
        return;
    }
    let modes: Vec<Mode> = ms.iter().map(|&m| mode_of(cx.modules[m], true)).collect();
    // A generated module added meanwhile is one module more, eager.
    cx.lazy.modules.resize(ms[0], Mode::Eager);
    cx.lazy.modules.extend(modes);
    plan_defs(cx, from);
}

/// What [`plan`] works out per definition, for those from `from` on.
fn plan_defs(cx: &mut Ctx, from: u32) {
    cx.lazy.fn_deferred.resize(from as usize, None);
    for i in from as usize..cx.info.len() {
        let known = matches!(cx.info[i], DefInfo::Fn(_)).then(|| decide(cx, DefId(i as u32)));
        cx.lazy.fn_deferred.push(known);
    }
    for i in from as usize..cx.info.len() {
        let d = DefId(i as u32);
        if let DefInfo::Adt(a) = &cx.info[i] {
            if let Some(TyKind::Adt(base, _)) = a.base.map(|b| cx.ty.kind(b)) {
                cx.lazy.subclasses.entry(*base).or_default().push(d);
            }
        }
        if let (DefInfo::Fn(f), true) = (&cx.info[i], defers(cx, d)) {
            let with_type = matches!(f.kind, FnKind::Method | FnKind::Ctor)
                || LAZY_PRELUDE_STATICS.contains(&f.name.as_str());
            if let (Some(o), true) = (f.owner, with_type) {
                cx.lazy.methods.entry(o).or_default().push(d);
            }
        }
    }
}

/// Is `d` a function or module constant checked only once checked code reaches it?
pub(crate) fn deferred(cx: &Ctx, d: DefId) -> bool {
    !cx.lazy.check_all && defers(cx, d)
}

/// [`deferred`], as far as [`reach`] is concerned: with `Lazy::check_all` the body is checked
/// anyway, and [`reach`] works out as before whether it would have been.
pub(super) fn defers(cx: &Ctx, d: DefId) -> bool {
    match cx.lazy.fn_deferred.get(d.0 as usize) {
        Some(Some(known)) => *known,
        _ => decide(cx, d),
    }
}

/// [`deferred`], worked out.
fn decide(cx: &Ctx, d: DefId) -> bool {
    if cx.lazy.modules.is_empty() {
        return false;
    }
    match &cx.info[d.0 as usize] {
        DefInfo::Fn(f) if f.source.is_none() => false,
        DefInfo::Fn(f) => match f.kind {
            FnKind::Free | FnKind::Static => match cx.lazy.modules[f.module] {
                Mode::Eager => false,
                Mode::Lazy => true,
                Mode::Helpers => !is_helper(&f.name) || RECORD_HOOKS.contains(&f.name.as_str()),
            },
            // A type's methods, where the type is declared (an `extend` of a prelude class
            // keeps its methods checked); an `extend` of a primitive type's.
            FnKind::Method | FnKind::Ctor => match f.owner {
                Some(o) => match &cx.info[o.0 as usize] {
                    DefInfo::Adt(a) => match cx.lazy.modules[a.module] {
                        Mode::Lazy => true,
                        Mode::Helpers => LAZY_PRELUDE_TYPES.contains(&a.qual_name.as_str()),
                        Mode::Eager => false,
                    },
                    _ => false,
                },
                None => primitive_extension(cx, f),
            },
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

/// Is `f` a method of an `extend` block on a number type, `bool`, `string` or a `velt/simd`
/// vector in a lazy std module (`extend u32 { rotateLeft(…) }`, a vector's operators), other
/// than a helper? Such a method is reached only by
/// a call or a function value naming it, or through an interface implementation that holds it
/// ([`impl_methods`]): no class's `any` table or subclass dispatch lists it.
fn primitive_extension(cx: &Ctx, f: &crate::defs::FnInfo<'_>) -> bool {
    f.kind == FnKind::Method
        && cx.lazy.modules[f.module] != Mode::Eager
        && !is_helper(&f.name)
        && f.this.as_ref().is_some_and(|t| {
            matches!(
                cx.ty.kind(t.ty),
                TyKind::Int(_) | TyKind::Float(_) | TyKind::Bool | TyKind::Str | TyKind::Simd(_)
            )
        })
}

/// The deferred methods held by interface implementations (`extend i32 implements I`, the
/// `Comparable` of every number type, a number converted to an interface): lowering calls them
/// through the implementation (a bound's method, a table of a `dyn` value), which no checked code
/// names.
fn impl_methods(cx: &Ctx, found: &mut Vec<DefId>) {
    for imp in &cx.impls {
        // Not a class's methods: those wait for the class to be used.
        let extension = |d: DefId| {
            matches!(&cx.info[d.0 as usize], DefInfo::Fn(f) if f.owner.is_none()) && defers(cx, d)
        };
        found.extend(imp.methods.iter().copied().filter(|&d| extension(d)));
    }
}

/// Check the deferred definitions that checked code reaches, and what their code reaches, until
/// nothing new is reached.
pub(crate) fn reach(cx: &mut Ctx) {
    if cx.lazy.modules.is_empty() {
        return;
    }
    // Defaults of fields and parameters may refer to them too: scanned once, first, except the
    // field defaults of a class whose methods are deferred, scanned once the class is used (the
    // type of `#tape: Tape | null = null` would make `Tape` used in every program).
    let mut sc = Scan::default();
    // The hooks the program's own code demands, checked first, as if eager: what they call is
    // found with the rest in the first round below.
    live::step(cx, &mut sc.found);
    for d in std::mem::take(&mut sc.found) {
        if defers(cx, d) {
            check_found(cx, d);
        }
    }
    loop {
        let defs_before = cx.info.len();
        // Those of the definitions made since the last time (generated code checked later,
        // `generated::phase`; std modules added below, `std_load`).
        let from = std::mem::replace(&mut cx.lazy.defaults_scanned, cx.info.len());
        for i in from..cx.info.len() {
            let lazy_class = cx.lazy.methods.contains_key(&DefId(i as u32));
            // A deferred function's parameter defaults are scanned with its body, once reached.
            let deferred_fn = defers(cx, DefId(i as u32));
            match &mut cx.info[i] {
                DefInfo::Fn(f) if !deferred_fn => {
                    for e in f.params.iter_mut().filter_map(|p| p.default.as_mut()) {
                        scan_expr(e, &mut sc);
                    }
                }
                DefInfo::Adt(a) if !lazy_class => scan_defaults(a, &mut sc),
                _ => {}
            }
        }
        let used: Vec<DefId> = (cx.lazy.used.iter())
            .filter(|d| cx.lazy.methods.contains_key(d) && !cx.lazy.scanned.contains(d))
            .copied()
            .collect();
        for d in used {
            cx.lazy.scanned.insert(d);
            if let DefInfo::Adt(a) = &mut cx.info[d.0 as usize] {
                scan_defaults(a, &mut sc);
            }
        }
        for i in 0..cx.info.len() {
            let d = DefId(i as u32);
            let done = match &cx.info[i] {
                DefInfo::Fn(_) => cx.defs[i].is_some(),
                DefInfo::Global(g) => g.state == BodyState::Done,
                _ => false,
            };
            let deferred_fn = defers(cx, d);
            // With `check_all`, what would not have been checked yet is not scanned yet.
            let done = done && !(cx.lazy.check_all && deferred_fn && !cx.lazy.reached.contains(&d));
            if !done || !cx.lazy.scanned.insert(d) {
                continue;
            }
            let module = match &cx.info[i] {
                DefInfo::Fn(f) => f.module,
                DefInfo::Global(g) => g.module,
                _ => continue,
            };
            let user = !cx.modules[module].is_std;
            let first_type = sc.types.len();
            let (first_conv, first_into) = (sc.conversions.len(), sc.into_any.len());
            // Text conversions count here only in the program's own modules, for the float hooks:
            // std code is checked whether or not the program uses it (the prelude's methods, and
            // what they call), and where lowering builds such std code and needs a float hook
            // that stayed unchecked, it calls the runtime. The integer hooks, which have no such
            // fallback, count conversions in live code, std's included ([`live`]).
            let texts_before = sc.texts.len();
            match (&mut cx.defs[i], &mut cx.info[i]) {
                (Some(hir::Def::Fn(f)), info) => {
                    if let (true, DefInfo::Fn(fi)) = (deferred_fn, info) {
                        for e in fi.params.iter_mut().filter_map(|p| p.default.as_mut()) {
                            scan_expr(e, &mut sc);
                        }
                    }
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
            // Only a non-std module's values put into an `any` count (std's own code puts its
            // records into `any` values in every program).
            let into: Vec<(TyId, TyId)> = sc.into_any.drain(first_into..).collect();
            if user && !cx.lazy.record_hooks {
                note_user_conversions(
                    cx,
                    &sc.types[first_type..],
                    &sc.conversions[first_conv..],
                    &into,
                );
            }
            if !user {
                sc.texts.truncate(texts_before);
            }
        }
        for t in std::mem::take(&mut sc.conversions) {
            if !cx.lazy.record_conversion {
                cx.lazy.record_conversion = holds_string_record(cx, t, &mut FxHashSet::default());
            }
        }
        used_types(cx, std::mem::take(&mut sc.types), &mut sc.found);
        if !cx.lazy.record_hooks && record_hooks_needed(cx) {
            cx.demand_std(velt_common::std_module!(lang_hooks_any_record));
            let hooks: Vec<DefId> = RECORD_HOOKS
                .iter()
                .filter_map(|h| cx.prelude_fn(h))
                .collect();
            if !hooks.is_empty() {
                cx.lazy.record_hooks = true;
                sc.found.extend(hooks);
                // They read the members of the object through `any` (std's own dynamic reads
                // don't add the member slots, `FnCx::note_dyn_members`).
                cx.note_dyn_members();
            }
        }
        impl_methods(cx, &mut sc.found);
        number_text::reach_hooks(cx, std::mem::take(&mut sc.texts), &mut sc.found);
        live::step(cx, &mut sc.found);
        let mut checked = false;
        for d in std::mem::take(&mut sc.found) {
            if defers(cx, d) {
                checked |= check_found(cx, d);
            }
        }
        // A class first used just now has its field defaults still to scan.
        let pending = (cx.lazy.used.iter())
            .any(|d| cx.lazy.methods.contains_key(d) && !cx.lazy.scanned.contains(d));
        // Modules added just now have their defaults and eager bodies still to scan.
        if !checked && !pending && cx.info.len() == defs_before {
            break;
        }
    }
}

/// Check the deferred definition `d`, which [`reach`] found, unless it is: whether it did.
fn check_found(cx: &mut Ctx, d: DefId) -> bool {
    if cx.lazy.check_all {
        return cx.lazy.reached.insert(d);
    }
    match &cx.info[d.0 as usize] {
        DefInfo::Fn(f) if f.state == BodyState::Unchecked => super::ensure_body(cx, d),
        DefInfo::Global(g) if g.state == BodyState::Unchecked => super::ensure_global(cx, d),
        _ => return false,
    }
    true
}

/// Record the types `todo` and their parts as met; each class or struct met for the first time
/// adds its deferred methods (and those of its base classes, its subclasses and its fields'
/// types) to `found`.
///
/// Its subclasses too: lowering's glue for a class value (`console.log`'s formatting,
/// `JSON.stringify`, a member of `any`) dispatches over every class in the program that extends
/// it (`velt_vir` `any_subclass_dispatch`, `json_descendants`), constructed or not, and so builds
/// their methods (`[INSPECT]`, `toJSON`, …).
fn used_types(cx: &mut Ctx, mut todo: Vec<TyId>, found: &mut Vec<DefId>) {
    let mut classes: Vec<DefId> = vec![];
    loop {
        if let Some(t) = todo.pop() {
            if !cx.lazy.types.insert(t) {
                continue;
            }
            let kind = cx.ty.kind(t).clone();
            todo.extend(crate::types::children(&kind));
            if let TyKind::Adt(d, _) = kind {
                classes.push(d);
            }
            continue;
        }
        let Some(d) = classes.pop() else { break };
        if !cx.lazy.used.insert(d) {
            continue;
        }
        if let Some(a) = cx.adt(d) {
            todo.extend(a.fields.iter().map(|f| f.ty));
            todo.extend(a.base);
        }
        classes.extend(cx.lazy.subclasses.get(&d).into_iter().flatten().copied());
        found.extend(cx.lazy.methods.get(&d).into_iter().flatten().copied());
    }
}

/// Does the program need [`RECORD_HOOKS`]: an explicit conversion of an `any` to a type holding
/// a record with string keys; for the conversions lowering's glue makes (a member written
/// through `any`), a non-std module putting a value whose members, elements or values hold such
/// a record into an `any`; or a non-std module with such a record converting a value of a type
/// parameter to or from an `any`? A program that converts nothing to a record does not check
/// them, whatever `any` values and records it has.
fn record_hooks_needed(cx: &Ctx) -> bool {
    cx.lazy.record_conversion || (cx.lazy.generic_any && cx.lazy.user_record)
}

/// The field defaults and redefined initializers of class or struct `a`.
fn scan_defaults(a: &mut crate::defs::AdtInfo<'_>, sc: &mut Scan) {
    for e in a.fields.iter_mut().filter_map(|f| f.default.as_mut()) {
        scan_expr(e, sc);
    }
    for e in a.redefined.iter_mut().filter_map(|r| r.init.as_mut()) {
        scan_expr(e, sc);
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
                    changes_held: false,
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
    /// The types its explicit conversions of an `any` give (`AnyTo`).
    conversions: Vec<TyId>,
    /// The `(value, interface)` types of its conversions to an interface value (`ToDyn`).
    into_any: Vec<(TyId, TyId)>,
    /// Types of the values it converts to text (`ToString`, `TemplateToString`).
    texts: Vec<TyId>,
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
            // A type argument is a used type: monomorphization builds the methods the callee
            // calls on it (`probe<Gadget>()` printing a `T`).
            sc.types.extend(targs);
        }
        E::AdtLit { type_args, .. } | E::Variant { type_args, .. } | E::New { type_args, .. } => {
            sc.types.extend(type_args)
        }
        E::Global(d) => sc.found.push(*d),
        E::Call {
            callee: Callee::Intrinsic(hir::Intrinsic::AnyTo | hir::Intrinsic::AnyToExact),
            ..
        } => sc.conversions.push(e.ty),
        E::ToDyn { expr, .. } => sc.into_any.push((expr.ty, e.ty)),
        E::Call {
            callee: Callee::Intrinsic(hir::Intrinsic::ToString | hir::Intrinsic::TemplateToString),
            args,
        } => sc.texts.extend(args.iter().map(|a| a.ty)),
        E::Call {
            callee: Callee::Dyn {
                method_type_args, ..
            },
            ..
        } => {
            sc.types.extend(method_type_args);
        }
        E::Call {
            callee:
                Callee::ParamMethod {
                    method_type_args,
                    iface_args,
                    ..
                },
            ..
        } => {
            sc.types.extend(method_type_args.iter().chain(iface_args));
        }
        _ => {}
    }
}

#[cfg(test)]
#[path = "lazy_std_tests.rs"]
mod tests;
