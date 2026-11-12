//! Standard-library code checked on demand (#1658, docs/internals/design/lazy-std.md). A program
//! loads far more std than it can run: the prelude alone imports `velt/lang/number/parse`,
//! `velt/encoding/json/reader` and `velt/time/*`. So the body of a std definition is *deferred*
//! ([`deferred`]): it is checked once it is *live* (`live.rs`: a superset of what lowering builds),
//! or once a checked call asks for its result (`body::returns::ret_of`). [`reach`] runs after the
//! bodies that aren't deferred, and works the live set out, checking what enters it. What it never
//! reaches stays unchecked: the program holds a stub for each (an empty body), listed in
//! `hir::Program::unchecked`, which lowering never builds (hir_encodings.md "Unchecked
//! definitions"), so a gap in the live set's rules is an ICE, not a changed program.
//!
//! Deferred: every function with a body and every module constant (not a module slot) of a std
//! module, unless a user module imports that module directly or uses it as its JSX runtime, or it
//! has top-level statements. Such a module is checked whole ([`Mode::whole`]): `velt check`
//! reports everything in what a program imports. A user module's code is always checked.
//!
//! Lowering calls some definitions by name, which no checked code refers to. The roots of the
//! live set say which ([`Mode::named`]): the free functions of the modules it looks into
//! ([`BY_NAME`]), and in the prelude ([`HELPERS`]) the compiler's helpers ([`is_helper`]). Two
//! kinds of hook lowering can do without stay unchecked until the program needs them:
//! [`RECORD_HOOKS`], for a conversion to a record ([`record_hooks_needed`]), and the float hooks,
//! for a text conversion in the program's own code or a float printed (`number_text.rs`). The
//! hooks whose table entry lists constructs (velt_common `hook_demand.rs`) are demanded by the
//! constructs of live code, and `hir::Program::hooks_demanded` tells lowering.
//!
//! All of std is checked with laziness off (`VELT_CHECK_ALL_STD=1`), as `velt check --std` and
//! veltc's `all_of_std_checks_without_diagnostics` do. The editor's analysis checks everything.

use rustc_hash::{FxHashMap, FxHashSet};

use crate::ctx::Ctx;
use crate::defs::{BodyState, DefInfo};
use crate::hir::{self, DefId, ExprKind as E, TyKind};

mod live;
mod number_text;
mod record_conversions;

use record_conversions::holds_string_record;

pub(crate) use live::{
    assume, demanded as hooks_demanded, finish as finish_live, live_defs, reach_again, too_late,
    Assumed,
};

/// Std modules (path prefixes) whose free functions lowering looks up by name (`velt_vir`
/// `STABLE_SORT`, the proxy and typed-array glue, the epoch sections' `pin`, `unpin` and `retire`,
/// `this_fn`'s glue, the `HostRef` hooks of host function calls) and calls: roots of the live set.
const BY_NAME: &[&str] = &[
    velt_common::std_module!(lang_sort),
    velt_common::std_module!(lang_proxy),
    velt_common::std_module!(lang_typed_array),
    velt_common::std_module!(sync_epoch),
    velt_common::std_module!(lang_this_fn),
    velt_common::std_module!(host),
];

/// The prelude's modules (path prefixes): lowering looks up some of their free functions by name
/// ([`is_helper`]), roots of the live set. The methods it looks up are live with their type.
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
            !h.fallback && h.constructs.is_empty() && !SCHED_HOOKS.contains(&qualified)
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
/// once [`record_hooks_needed`], live or not. Lowering leaves a conversion whose hooks are
/// unchecked the `TypeError` it was without them.
pub(crate) const RECORD_HOOKS: &[&str] = &[
    velt_common::std_hook!(any_to_record_any),
    velt_common::std_hook!(any_to_record),
];

/// Which free functions of a module lowering looks up by name.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Named {
    None,
    /// The compiler's helpers ([`is_helper`]).
    Helpers,
    All,
}

/// How a module's definitions are checked.
#[derive(Clone, Copy)]
struct Mode {
    /// Every body, before [`reach`]: it isn't deferred.
    whole: bool,
    named: Named,
}

/// What is checked on demand in one check.
#[derive(Clone)]
pub(crate) struct Lazy {
    /// Off for the editor's analysis (`ide`), which checks everything, unless it has a load
    /// callback: then `check_all`.
    pub on: bool,
    /// Check every body (the editor's analysis), and still work the live set out, for one demand
    /// set with `velt check` (`std_load`): nothing is [`deferred`].
    pub check_all: bool,
    /// Per module: how it is checked (empty while off).
    modules: Vec<Mode>,
    /// Per class: the classes that extend it directly.
    subclasses: FxHashMap<DefId, Vec<DefId>>,
    /// Has checked code of a non-std module a type holding a record with string keys in it?
    user_record: bool,
    /// Does checked code of a non-std module convert a value of a type parameter to or from an
    /// `any` (`<T>(x: any): T`), which may be a record at run time?
    generic_any: bool,
    /// Does live code convert an `any` to a type holding a record with string keys (an `AnyTo`),
    /// or a non-std module put a value whose members, elements or values hold one into an `any`
    /// (where a member written through the `any` converts to it)?
    record_conversion: bool,
    /// Have [`RECORD_HOOKS`] been reached?
    record_hooks: bool,
    /// Have [`SCHED_HOOKS`] been reached?
    sched_hooks: bool,
    /// The program's code converts a value that may hold a float to text: the float hooks are
    /// reached (`velt_common::std_hooks::ON_DEMAND_FLOAT`).
    float_text: bool,
    /// Live code prints a float: `console.log`'s float writer is reached
    /// (`velt_common::std_hooks::ON_DEMAND_WRITE`).
    float_write: bool,
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
            modules: vec![],
            subclasses: FxHashMap::default(),
            user_record: false,
            generic_any: false,
            record_conversion: false,
            record_hooks: false,
            sched_hooks: false,
            float_text: false,
            float_write: false,
            live: live::Live::new(),
        }
    }

    /// A generated module added during the check (`Ctx::add_module`): checked eagerly, as any
    /// non-std module.
    pub(crate) fn add_module(&mut self) {
        if !self.modules.is_empty() {
            self.modules.push(Mode {
                whole: true,
                named: Named::None,
            });
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

/// Is the std module at `path` one lowering calls into by name ([`BY_NAME`], [`HELPERS`])?
pub(crate) fn called_by_name(path: &str) -> bool {
    (BY_NAME.iter().chain(HELPERS)).any(|p| in_module(path, p))
}

/// Is `path` the std module or directory `p`, or inside it (`std/lang/sort` is not in
/// `std/lang/so`)?
fn in_module(path: &str, p: &str) -> bool {
    path.strip_prefix(p)
        .is_some_and(|rest| rest.is_empty() || p.ends_with('/') || rest.starts_with('/'))
}

/// How module `m` is checked; `lazy`: the program does not ask for it whole.
fn mode_of(m: &crate::SourceModule, lazy: bool) -> Mode {
    let in_list = |list: &[&str]| list.iter().any(|p| in_module(&m.path, p));
    Mode {
        whole: !(lazy && m.is_std && crate::module_scripts::script_fns(m).is_empty()),
        named: match m.is_std {
            true if in_list(BY_NAME) => Named::All,
            true if in_list(HELPERS) => Named::Helpers,
            _ => Named::None,
        },
    }
}

/// [`plan`] for std modules `ms`, added during the check (`crate::std_load`), whose definitions
/// start at `from`. No user module imports them (it would have loaded them).
pub(crate) fn plan_added(cx: &mut Ctx, ms: &[usize], from: u32) {
    if cx.lazy.modules.is_empty() {
        return;
    }
    let modes: Vec<Mode> = ms.iter().map(|&m| mode_of(cx.modules[m], true)).collect();
    // A generated module added meanwhile is one module more, whole.
    let whole = Mode {
        whole: true,
        named: Named::None,
    };
    cx.lazy.modules.resize(ms[0], whole);
    cx.lazy.modules.extend(modes);
    plan_defs(cx, from);
}

/// What [`plan`] works out per definition, for those from `from` on.
fn plan_defs(cx: &mut Ctx, from: u32) {
    for i in from as usize..cx.info.len() {
        if let DefInfo::Adt(a) = &cx.info[i] {
            if let Some(TyKind::Adt(base, _)) = a.base.map(|b| cx.ty.kind(b)) {
                cx.lazy
                    .subclasses
                    .entry(*base)
                    .or_default()
                    .push(DefId(i as u32));
            }
        }
    }
}

/// Is `d` a function or module constant checked only once it is live, or a checked call asks for
/// its result?
pub(crate) fn deferred(cx: &Ctx, d: DefId) -> bool {
    if cx.lazy.check_all || cx.lazy.modules.is_empty() {
        return false;
    }
    let whole = |m: usize| cx.lazy.modules[m].whole;
    match &cx.info[d.0 as usize] {
        DefInfo::Fn(f) => f.source.is_some() && !whole(f.module),
        // Module `let`s and static fields are module state.
        DefInfo::Global(g) => g.src.owner.is_none() && !g.mutable && !g.stored && !whole(g.module),
        _ => false,
    }
}

/// Work the live set out, with the definitions it adds checked, and the hooks that wait for what
/// the program does, until nothing new is reached. Each definition is walked once.
pub(crate) fn reach(cx: &mut Ctx) {
    if cx.lazy.modules.is_empty() {
        return;
    }
    loop {
        let defs_before = cx.info.len();
        let texts = live::step(cx);
        let mut found = vec![];
        if !cx.lazy.record_hooks && record_hooks_needed(cx) && cx.lazy.live.finishing {
            cx.lazy.live.late.record_conversion = true;
        } else if !cx.lazy.record_hooks && record_hooks_needed(cx) {
            cx.demand_std(velt_common::std_module!(lang_hooks_any_record));
            let hooks: Vec<DefId> = RECORD_HOOKS
                .iter()
                .filter_map(|h| cx.prelude_fn(h))
                .collect();
            if !hooks.is_empty() {
                cx.lazy.record_hooks = true;
                found.extend(hooks);
                // They read the members of the object through `any` (std's own dynamic reads
                // don't add the member slots, `FnCx::note_dyn_members`).
                cx.note_dyn_members();
            }
        }
        number_text::reach_hooks(cx, texts, &mut found);
        number_text::reach_writer(cx, &mut found);
        // A std function C calls (`@export`) is live though no checked code refers to it.
        found.extend(cx.c_exports.iter().map(|e| e.def));
        sched_hooks(cx, &mut found);
        let mut checked = false;
        for d in found {
            checked |= live::mark(cx, d);
            checked |= check_found(cx, d);
        }
        // Modules added just now have their roots still to walk.
        if !checked && cx.info.len() == defs_before {
            break;
        }
    }
}

/// The executor in Velt's hooks (velt_common `std_hooks.rs`, `sched_*`), which lowering calls in
/// place of the runtime's executor when the compiler was started with `VELT_SCHEDULER=std`:
/// their module is loaded and they are live then, whatever the program does, and never
/// otherwise.
const SCHED_HOOKS: &[&str] = &[
    velt_common::std_hook!(sched_block_on),
    velt_common::std_hook!(sched_spawn),
    velt_common::std_hook!(sched_yield),
    velt_common::std_hook!(sched_finish),
    velt_common::std_hook!(sched_box),
    velt_common::std_hook!(sched_start),
    velt_common::std_hook!(sched_detach),
    velt_common::std_hook!(sched_handled),
    velt_common::std_hook!(sched_await_tick),
    velt_common::std_hook!(sched_await_ready),
    velt_common::std_hook!(sched_microtask),
    velt_common::std_hook!(sched_latch_new),
    velt_common::std_hook!(sched_latch_free),
    velt_common::std_hook!(sched_latch_open),
    velt_common::std_hook!(sched_latch_wait),
    velt_common::std_hook!(sched_keep_alive_acquire),
    velt_common::std_hook!(sched_keep_alive_release),
    velt_common::std_hook!(sched_unref_due),
    velt_common::std_hook!(sched_tla_at),
    velt_common::std_hook!(sched_spawn_fut),
    velt_common::std_hook!(sched_pinned),
    velt_common::std_hook!(sched_sleep),
    velt_common::std_hook!(sched_timer_new),
    velt_common::std_hook!(sched_timer_turn),
    velt_common::std_hook!(sched_timer_again),
    velt_common::std_hook!(sched_timer_clear),
    velt_common::std_hook!(sched_timer_free),
    velt_common::std_hook!(sched_uncaught_now),
    velt_common::std_hook!(sched_module_state),
    velt_common::std_hook!(sched_module_block),
    velt_common::std_hook!(sched_module_ready),
    velt_common::std_hook!(sched_task_id),
];

/// The debug cell check's hooks under the executor in Velt (sched_cells.velt): only a debug
/// compiler emits the checks (velt_vir `LowerOptions::cell_checks`), so only it loads them.
const SCHED_CELL_HOOKS: &[&str] = &[
    velt_common::std_hook!(sched_cell_new),
    velt_common::std_hook!(sched_cell_use),
    velt_common::std_hook!(sched_cell_give),
    velt_common::std_hook!(sched_cell_copy),
    velt_common::std_hook!(sched_cell_free),
];

/// Adds [`SCHED_HOOKS`] to `found` the first time, when the executor in Velt is selected and
/// live code runs on it (`live::Live::tasks`: async code, or an extern the executor serves). A
/// program without such code doesn't load the executor; lowering then keeps the runtime's
/// module state (velt_vir `module_state.rs`), and never calls a task or timer hook (#2649 P3(a)).
fn sched_hooks(cx: &mut Ctx, found: &mut Vec<DefId>) {
    let wanted = cx.lazy.live.tasks && velt_common::std_hooks::std_scheduler();
    if cx.lazy.sched_hooks || !wanted {
        return;
    }
    if cx.lazy.live.finishing {
        // Reached by what the passes after the check made: the next round has it.
        cx.lazy.live.late.tasks = true;
        return;
    }
    cx.lazy.sched_hooks = true;
    cx.demand_std(velt_common::std_module!(lang_hooks_sched));
    found.extend(SCHED_HOOKS.iter().filter_map(|h| cx.prelude_fn(h)));
    if cfg!(debug_assertions) {
        cx.demand_std(velt_common::std_module!(lang_hooks_sched_cells));
        found.extend(SCHED_CELL_HOOKS.iter().filter_map(|h| cx.prelude_fn(h)));
    }
}

/// Check the definition `d` if it is deferred and unchecked: whether it did.
fn check_found(cx: &mut Ctx, d: DefId) -> bool {
    if !deferred(cx, d) {
        return false;
    }
    let unchecked = match &cx.info[d.0 as usize] {
        DefInfo::Fn(f) => f.state == BodyState::Unchecked,
        DefInfo::Global(g) => g.state == BodyState::Unchecked,
        _ => false,
    };
    if !unchecked {
        return false;
    }
    // The passes after the bodies' check have run: they would not see this body. The next round
    // has it live from the start (`live::finish`).
    if cx.lazy.finishing() {
        if let Some(name) = live::def_name(cx, d).filter(|_| cx.collected(d)) {
            cx.lazy.live.late.defs.push((d, name));
        } else {
            cx.err(
                format!("ICE: definition {} became live after the passes", d.0),
                velt_common::Span::DUMMY,
            );
        }
        return false;
    }
    match cx.global(d).is_some() {
        true => super::ensure_global(cx, d),
        false => super::ensure_body(cx, d),
    }
    true
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

#[cfg(test)]
#[path = "lazy_std_tests.rs"]
mod tests;
