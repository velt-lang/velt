//! What a splice (`kept.rs`) does with the answers lowering keeps between functions: the answers
//! about one function, or about the whole program, that it computed once and read while it
//! lowered other functions. Callers in files the edit leaves alone keep code built with the old
//! answers, so every memo of `Cx` and `Layouts` has a rule, one of three.
//!
//! **Taken out, answered anew from the new program, and compared** (a different answer lowers
//! the program whole); the answers a function's body decides:
//! - `quiet`: whether a function runs user code (quiet.rs), from its body and its callees'.
//! - `cpu_checks.known`: whether a call is a CPU check this build answers (cpu_checks.rs), from
//!   the body of a function that only returns checks; transitive, so the whole map is compared.
//! - `asyncs`: an async function's state (async_fn/), from its body.
//! - `lay.envs`: a closure's environment (layout.rs), from its captures.
//! - `by_ref_params`: the closures whose scalar params are passed by pointer (async_fn/sync.rs).
//! - `fn_writes`, `callee_stores`, `assigned_fields`, `reassigned_slots`, `field_writes`: what
//!   functions store and what code assigns (write_effects.rs, replaceable.rs, index_order.rs,
//!   module_state.rs), from every body.
//! - `value_params`: the params that may reach a function value (`velt_sema::value_params`), from
//!   every body.
//! - `no_await_steps` (passes.rs): whether nothing can observe an `await` step, from the runtime
//!   functions the program calls; a splice that calls an observer where the kept program had
//!   none is refused. (An edit that removes the last observer keeps the steps a cold build
//!   leaves out: nothing can observe them, so the program does the same, with more code.)
//! - `uses_placement`, `facts`, `boxing` (kept.rs `relower`): whether a task is pinned, what
//!   lowering learned of the program, the counted types.
//!
//! **Taken out**, made again for the edited function alone, which only the edited files'
//! functions name: `lay.value_wraps` (an async function's promise wrapper, from its state),
//! `lay.gen_tables` (a generator's table, of its functions).
//!
//! **Depends only on declarations, the types, std or the options**, which a body edit leaves
//! alone (warm sema refuses an edit of a declaration, and compares the interfaces and
//! implementations):
//! - `tracked`, `tracked_size`: caller-tracking is decided for std functions only.
//! - `tables`, `static_atomics`, `module_desc`: from the module globals and the slots sema gave
//!   them (a changed slot is a changed declaration).
//! - `dyn_modes_memo`: an interface slot's pass modes, which the interface check compares.
//! - `iface_impls`, `lay.adapters`: from `hir::Program::impls`, which the interface check
//!   compares.
//! - `ext_memo`, `exists_memo`, `own_clones`, `honoured_clones`, `dispose_types`, `value_ids`,
//!   `anon`, `shapes`: from types, `extend` blocks and class methods.
//! - `generated_canon`: `hir::Program::generated`, which warm sema keeps equal.
//! - `glue_slots`: a dev build keeps every table entry (`GlueSlots::keep_all`).
//! - `lay` but the three above: layouts per type and class hierarchy (`value`, `views`,
//!   `objs`, `boxes`, `headers`, `extended`, `class_ids`, `fn_wrapper_env`, `array`, `text`,
//!   `closure`, `dyn_value`, `all_wrap`, `settle_wraps`, `race_boxes`, `widen_boxes`,
//!   `adopt_boxes`, `vtables`, `slot_tables`, `generic_slots`, `drop_memo`, `foreign`).
//! - `prelude_proxy`, `view_fn`, `std_fns`, `prelude_error`, `prelude_type_error`,
//!   `tla_marks`, `ffi_hooks`, `sched_tasks`: std's definitions.
//! - `native_inits`, `c_symbols`, `c_globals`, `system_aggs`, `reg_slot_agg`, `fut_header`:
//!   `declare` items and C layouts.
//! - `types`, `aggs`, `externs`, `extern_map`, `statics`, `static_map`, `str_objects`,
//!   `symbol_records`, `numbered`, `callees`, `callee_ids`: tables keyed by content, which only
//!   grow.
//! - `cell_checks`, `target`, `jit`, `facts_only`: the options and the pass.
//!
//! **Lowering's own state** of the functions it builds, rebuilt with them or emptied once a
//! pass ends: `funcs`, `work_map`, `queue`, `iface_glues`, `fn_tables`, `building`,
//! `built_log`, `unique_params`, `declared_unique`, `deferred`, `deferred_new`, `versioned`,
//! `cur_*`, `user_site`, `fn_sites`, `closure_parents` (each closure's entry is written again
//! when it is lowered again), `locs`, `debug`, `map_fields`, `unwinding`. And what feeds
//! diagnostics (any one refuses the splice): `unavailable`, `unreduced`, `c_clashes`,
//! `c_clashes_reported`, `std_calls`, `nothing_clashes`, `atomic_copies`, `share_sites`,
//! `shared_locks`, `share_from`.

use rustc_hash::{FxHashMap, FxHashSet};
use velt_sema::hir::{DefId, TyId};

use super::super::{async_fn, index_order, module_state, quiet, replaceable, write_effects, Cx};
use crate::vir;

/// What [`Cx::forget_answers`] took out.
pub(super) struct Asked {
    quiet: Vec<((DefId, quiet::Quiet), bool)>,
    states: Vec<((DefId, Vec<TyId>), Shape)>,
    envs: Vec<((DefId, Vec<TyId>), Shape)>,
    by_ref: FxHashSet<(DefId, Vec<TyId>)>,
    cpu_checks: FxHashMap<DefId, Option<bool>>,
    /// The runtime functions that observe `await` steps the program called (`no_await_steps`).
    observers: usize,
    whole: Whole,
    /// The files edited (`edited`).
    files: Vec<velt_common::FileId>,
}

/// Is `d` a function defined in one of `files`?
fn edited(hir: &velt_sema::hir::Program, files: &[velt_common::FileId], d: DefId) -> bool {
    matches!(hir.def(d), velt_sema::hir::Def::Fn(f) if files.contains(&f.span.file))
}

/// The whole-program answers, as they were.
#[derive(Default)]
struct Whole {
    reassigned_slots: Option<FxHashSet<DefId>>,
    assigned_fields: Option<index_order::AssignedFields>,
    fn_writes: Option<write_effects::FnWrites>,
    field_writes: Option<std::rc::Rc<replaceable::FieldWrites>>,
    value_params: Option<std::rc::Rc<velt_sema::value_params::ValueParams>>,
    callee_stores: FxHashMap<u32, Option<std::rc::Rc<replaceable::Stores>>>,
}

/// What code outside a function reads of a layout it owns (an async function's state, a
/// closure's environment): size, alignment, and each field's offset and type (an aggregate by
/// its name and size), with, for a state, where each input goes and the result's type.
#[derive(PartialEq)]
struct Shape {
    size: u32,
    align: u32,
    fields: Vec<(String, u32)>,
    inputs: Vec<Option<u32>>,
    result: Option<vir::Ty>,
}

impl Cx<'_> {
    fn shape(&self, agg: vir::AggId) -> Shape {
        let layout = &self.aggs[agg.0 as usize];
        let ty = |t: &vir::Ty| match t {
            vir::Ty::Agg(a) => {
                let l = &self.aggs[a.0 as usize];
                format!("{} size={} align={}", l.name, l.size, l.align)
            }
            t => format!("{t:?}"),
        };
        Shape {
            size: layout.size,
            align: layout.align,
            fields: layout.fields.iter().map(|(t, o)| (ty(t), *o)).collect(),
            inputs: vec![],
            result: None,
        }
    }

    fn state_shape(&self, info: &async_fn::AsyncInfo) -> Shape {
        Shape {
            inputs: info.inputs.clone(),
            result: Some(info.result),
            ..self.shape(info.state)
        }
    }

    /// Take out the answers lowering keeps about the functions `edited` decides, and about the
    /// whole program (module docs), so that lowering the edited files again answers anew.
    pub(super) fn forget_answers(&mut self, files: &[velt_common::FileId]) -> Asked {
        let hir = self.hir;
        let edited = |d: DefId| edited(hir, files, d);
        let quiet: Vec<_> = (self.quiet.iter())
            .filter(|((d, _), _)| edited(*d))
            .map(|(k, v)| (*k, *v))
            .collect();
        self.quiet.retain(|(d, _), _| !edited(*d));
        let states: Vec<_> = (self.asyncs.iter())
            .filter(|((d, _), _)| edited(*d))
            .map(|(k, info)| (k.clone(), self.state_shape(info)))
            .collect();
        self.asyncs.retain(|(d, _), _| !edited(*d));
        let envs: Vec<_> = (self.lay.envs.iter())
            .filter(|((d, _), _)| edited(*d))
            .map(|(k, a)| (k.clone(), self.shape(*a)))
            .collect();
        self.lay.envs.retain(|(d, _), _| !edited(*d));
        let by_ref: FxHashSet<_> = (self.by_ref_params.iter())
            .filter(|(d, _)| edited(*d))
            .cloned()
            .collect();
        self.by_ref_params.retain(|(d, _)| !edited(*d));
        let cpu_checks = std::mem::take(&mut self.cpu_checks.known);
        let observers = self.observers();
        self.lay.value_wraps.retain(|(d, _), _| !edited(*d));
        self.lay.gen_tables.retain(|(d, _), _| !edited(*d));
        let whole = Whole {
            reassigned_slots: self.reassigned_slots.take(),
            assigned_fields: self.assigned_fields.take(),
            fn_writes: self.fn_writes.take(),
            field_writes: self.field_writes.take(),
            value_params: self.value_params.take(),
            callee_stores: std::mem::take(&mut self.callee_stores),
        };
        Asked {
            quiet,
            states,
            envs,
            by_ref,
            cpu_checks,
            observers,
            whole,
            files: files.to_vec(),
        }
    }

    /// The answers taken by [`Cx::forget_answers`] are what lowering answers now; why not.
    pub(super) fn same_answers(&mut self, asked: Asked) -> Result<(), String> {
        let name = |cx: &Self, d: DefId| cx.fn_def(d).name.clone();
        for ((d, level), was) in asked.quiet {
            if self.quiet_as(d, level) != was {
                let name = name(self, d);
                return Err(format!("the edit changes whether `{name}` runs user code"));
            }
        }
        for (key, was) in asked.states {
            let now = self.asyncs.get(&key).map(|info| self.state_shape(info));
            if now.as_ref() != Some(&was) {
                let name = name(self, key.0);
                return Err(format!("the edit changes the state of async `{name}`"));
            }
        }
        for (key, was) in asked.envs {
            // An environment not built again belongs to a closure the edit removed.
            let now = self.lay.envs.get(&key).map(|a| self.shape(*a));
            if now.is_some_and(|now| now != was) {
                let name = name(self, key.0);
                return Err(format!("the edit changes the captures of `{name}`"));
            }
        }
        let hir = self.hir;
        let now: FxHashSet<_> = (self.by_ref_params.iter())
            .filter(|(d, _)| edited(hir, &asked.files, *d))
            .cloned()
            .collect();
        if now != asked.by_ref {
            return Err("the edit changes which closures take their params by pointer".into());
        }
        for (d, was) in asked.cpu_checks {
            if self.cpu_check(d) != was {
                let name = name(self, d);
                return Err(format!(
                    "the edit changes whether `{name}` is a known CPU check"
                ));
            }
        }
        if self.no_await_steps && self.observers() > asked.observers {
            return Err("the edit makes `await` steps observable".into());
        }
        self.same_whole(asked.whole)
    }

    /// How many of the runtime functions that observe `await` steps the program calls
    /// (async_fn/await_order.rs: a runtime function is declared once something calls it).
    fn observers(&self) -> usize {
        (async_fn::await_order::OBSERVERS.iter())
            .filter(|s| self.extern_map.contains_key(**s))
            .count()
    }

    /// The whole-program answers, computed again from the new program, are the old ones.
    fn same_whole(&mut self, was: Whole) -> Result<(), String> {
        let hir = self.hir;
        let differs = |what: &str| Err(format!("the edit changes {what}, which callers read"));
        if let Some(was) = was.reassigned_slots {
            let now = (self.reassigned_slots.take())
                .unwrap_or_else(|| module_state::reassigned_slots(hir));
            if now != was {
                return differs("which module slots code assigns as a whole");
            }
            self.reassigned_slots = Some(now);
        }
        if let Some(was) = was.assigned_fields {
            let now =
                (self.assigned_fields.take()).unwrap_or_else(|| index_order::assigned_fields(hir));
            if now != was {
                return differs("which fields code assigns");
            }
            self.assigned_fields = Some(now);
        }
        if let Some(was) = was.fn_writes {
            let now = (self.fn_writes.take()).unwrap_or_else(|| write_effects::fn_writes(hir));
            if now != was {
                return differs("what a function stores");
            }
            self.fn_writes = Some(now);
        }
        if let Some(was) = was.field_writes {
            let now = match self.field_writes.take() {
                Some(now) => now,
                None => std::rc::Rc::new(self.scan_field_writes()),
            };
            if *now != *was {
                return differs("which fields are written after their object is made");
            }
            self.field_writes = Some(now);
        }
        if let Some(was) = was.value_params {
            let now = match self.value_params.take() {
                Some(now) => now,
                None => std::rc::Rc::new(velt_sema::value_params::of_program(hir)),
            };
            if *now != *was {
                return differs("which params may reach a function value");
            }
            self.value_params = Some(now);
        }
        for (callee, was) in was.callee_stores {
            let now = self.stores_of(replaceable::CalleeId::of(callee));
            if now.as_deref() != was.as_deref() {
                let def = self.callees[callee as usize].0;
                let name = self.fn_def(def).name.clone();
                return differs(&format!("what `{name}` stores"));
            }
        }
        Ok(())
    }
}
