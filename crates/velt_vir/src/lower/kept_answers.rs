//! What a splice (`kept.rs`) does with the answers lowering keeps between functions: the answers
//! about one function, or about the whole program, that it computed once and read while it
//! lowered other functions. Callers in files the edit leaves alone keep code built with the old
//! answers, so each answer has a rule:
//!
//! - **taken out and compared**: answers about an edited function (whether it runs user code,
//!   `quiet.rs`; an async function's state, `async_fn/`; a closure's environment, `layout.rs`;
//!   whether a closure's scalar params are passed by pointer, `async_fn/sync.rs`) and answers
//!   about the whole program, computed from every body (what each function stores,
//!   `write_effects.rs`; the fields, slots and globals some code assigns, `index_order.rs`,
//!   `module_state.rs`, `replaceable.rs`; what direct callees store; the params that may reach a
//!   function value, `velt_sema::value_params`). Lowering the edited files again answers anew;
//!   a different answer lowers the program whole.
//! - **taken out**: what is built from such an answer for an edited function alone (the
//!   promise-value wrapper of an async function, a generator's table), which only the edited
//!   files' functions name.
//! - **depends only on declarations, or on std**, which a body edit leaves alone: the rest
//!   (type layouts, vtables, interface pass modes, the caller-tracking std functions).

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
        self.same_whole(asked.whole)
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
