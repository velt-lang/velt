//! Std modules added in the middle of a check (#2161 (a1), docs/internals/design/lazy-std.md
//! "Loading on demand"). When [`crate::body::lazy_std::reach`] finds a hook needed whose module the
//! program has not loaded ([`Ctx::demand_std`]), it asks the driver's load callback ([`LoadFn`]),
//! runs the source rewrites on what comes back ([`rewrite`]), and adds it to the check: declared,
//! collected ([`crate::collect::collect_added`]) and its eager bodies checked
//! ([`crate::body::check_added`]), as if it had been there from the start.
//!
//! The modules outlive every `Ctx` ([`StdLoads`]), and every check of the program: the driver's
//! rounds keep it too. A later round starts as the first did and demands again: it takes
//! the kept batches, in the order they were served, without asking the driver. A collected
//! definition keeps its `DefId` while the modules come in at the same points; a round that assumes
//! definitions demands some earlier, before closures that took numbers the last time, so there a
//! definition is found again by name (`Assumed::defs`), and a hint by `DefId` holds only where its
//! span matches (`throws/hints.rs`).
//!
//! The velt_loader rule (`std_index`) keeps what such a module adds local to it, so the rewrites
//! see the batch alone. `asserted_targets`, `global_this`, `defined_accessors` and `defined_props`
//! are not run: they rewrite only non-std modules. `module_chunks` is not either: the rule excludes top-level
//! statements.

use std::sync::{Mutex, OnceLock};

use rustc_hash::{FxHashMap, FxHashSet};
use velt_common::{Diagnostic, Span};

use crate::ctx::Ctx;
use crate::SourceModule;

/// What the driver's loader adds for a demand.
#[derive(Default)]
pub struct StdBatch {
    /// The modules, new to the load, in load order.
    pub modules: Vec<SourceModule>,
    /// Imports it resolved in modules loaded earlier (a re-export skipped until a name of it was
    /// asked for): module path, specifier, target path.
    pub imports: Vec<(String, String, String)>,
}

/// How the driver loads during a check: given canonical std module paths, those modules and what
/// they import that its load has not loaded yet. `None`: nothing can be loaded (the reasons are
/// the driver's). Called on sema's thread, once per module for all the check's rounds.
pub type LoadFn<'a> = dyn Fn(&[String]) -> Option<StdBatch> + Sync + 'a;

/// The most batches one check adds: more than std has modules, and each batch holds one at least.
const SLOTS: usize = 1024;

/// A served batch, rewritten, with what the rewrites tell the checker.
pub(crate) struct Batch {
    modules: Vec<SourceModule>,
    imports: Vec<(String, String, String)>,
    diags: Vec<Diagnostic>,
    ns_private: Vec<crate::namespaces::Private>,
    local_fns: FxHashSet<Span>,
    all_fns: FxHashSet<Span>,
    unsafe_fns: FxHashSet<Span>,
    has_overloads: bool,
    sig_spans: FxHashMap<Span, Span>,
    methods: FxHashSet<String>,
    adapted: FxHashSet<crate::overload_adapter::Adapted>,
    blocked: crate::recursive_aliases::Blocked,
    kept: FxHashSet<Span>,
    groups: Vec<crate::nested_closures::ClosureGroup>,
    calls_throw: bool,
}

/// The batches served for a program, in order: they outlive every check of it. A driver that
/// checks once may make one per check.
pub struct StdLoads {
    slots: Vec<OnceLock<Box<Batch>>>,
    /// Per demanded or served module path: its batch, `None` when it could not be loaded; and how
    /// many batches there are.
    served: Mutex<(FxHashMap<String, Option<usize>>, usize)>,
}

impl Default for StdLoads {
    fn default() -> Self {
        StdLoads {
            slots: (0..SLOTS).map(|_| OnceLock::new()).collect(),
            served: Default::default(),
        }
    }
}

/// How a check loads what it demands: the driver's callback, and what it served so far.
#[derive(Clone, Copy)]
pub struct Loads<'a> {
    pub run: &'a LoadFn<'a>,
    pub kept: &'a StdLoads,
}

impl<'a> Loads<'a> {
    /// The batch that holds module `path`, asking the driver the first time.
    fn serve(&self, path: &str) -> Option<usize> {
        let mut served = self.kept.served.lock().ok()?;
        if let Some(known) = served.0.get(path) {
            return *known;
        }
        let batch = (self.run)(&[path.to_string()]);
        let batch = batch.filter(|b| !b.modules.is_empty() || !b.imports.is_empty());
        let Some(batch) = batch else {
            served.0.insert(path.to_string(), None);
            return None;
        };
        let i = served.1;
        let batch = Box::new(rewrite(batch));
        for m in &batch.modules {
            served.0.insert(m.path.clone(), Some(i));
        }
        let holds = batch.modules.iter().any(|m| m.path == path);
        let kept = self.kept.slots.get(i).is_some_and(|s| s.set(batch).is_ok());
        assert!(
            kept,
            "ICE: more than {SLOTS} batches of std modules in a check"
        );
        served.1 = i + 1;
        if !holds {
            served.0.insert(path.to_string(), None);
            return None;
        }
        Some(i)
    }

    fn batch(&self, i: usize) -> &'a Batch {
        (self.kept.slots[i].get()).expect("ICE: a std batch used before it was served")
    }

    /// The batches a `Ctx` that added `added` of them holds.
    fn batches(&self, added: usize) -> impl Iterator<Item = &'a Batch> {
        (self.kept.slots[..added].iter()).filter_map(|s| s.get().map(|b| &**b))
    }
}

/// The rewrites `check_timed` runs on a program, on batch `b` alone (module docs).
fn rewrite(b: StdBatch) -> Batch {
    let StdBatch {
        mut modules,
        imports,
    } = b;
    let mut diags = vec![];
    diags.extend(crate::call_sigs::check(&modules));
    let (flat, ns_private, d) = crate::namespaces::flatten(&modules);
    diags.extend(d);
    modules = flat.unwrap_or(modules);
    let (merged, d) = crate::interface_merge::rewrite(&modules);
    diags.extend(d);
    modules = merged.unwrap_or(modules);
    let (callable, d) = crate::call_sigs::rewrite(&modules);
    diags.extend(d);
    modules = callable.unwrap_or(modules);
    let (mut local_fns, mut all_fns, mut unsafe_fns) = Default::default();
    if let Some(l) = crate::generic_arrows::lift(&modules) {
        (local_fns, all_fns, unsafe_fns) = (l.local_fns, l.all_fns, l.unsafe_fns);
        modules = l.modules;
    }
    let overloaded = crate::overloads::rewrite(&modules);
    let has_overloads = overloaded.is_some();
    let (mut sig_spans, mut methods, mut adapted) = Default::default();
    if let Some(o) = overloaded {
        (sig_spans, methods, adapted) = (o.sig_spans, o.methods, o.adapted);
        modules = o.modules;
    }
    let (aliased, blocked) = crate::recursive_aliases::rewrite(&modules);
    modules = aliased.unwrap_or(modules);
    let (mut kept, mut groups) = Default::default();
    if let Some(c) = crate::nested_closures::rewrite(&modules) {
        diags.extend(c.diags);
        (kept, groups) = (c.kept, c.groups);
        modules = c.modules;
    }
    let calls_throw = crate::ts_protocol::calls_throw(&modules);
    Batch {
        modules,
        imports,
        diags,
        ns_private,
        local_fns,
        all_fns,
        unsafe_fns,
        has_overloads,
        sig_spans,
        methods,
        adapted,
        blocked,
        kept,
        groups,
        calls_throw,
    }
}

impl Ctx<'_> {
    /// Was `d` made by a collection, the program's or that of added modules, rather than by a
    /// body's check? A next round can find such a definition again (`Assumed::defs`).
    pub(crate) fn collected(&self, d: crate::hir::DefId) -> bool {
        (d.0 as usize) < self.collected_defs
            || self.added_defs.iter().any(|&(a, b)| (a..b).contains(&d.0))
    }
}

impl<'m> Ctx<'m> {
    /// A hook of std module `path` is needed: add the module to the check unless it is loaded.
    /// Without a load callback, or when it cannot be loaded, the module stays missing, and the
    /// hook's lookup (`Ctx::std_def`) finds nothing.
    pub(crate) fn demand_std(&mut self, path: &str) {
        if crate::collect::exports::std_module(self, path).is_some() {
            return;
        }
        // The passes after the bodies' check have run: they would not see what this adds.
        if self.lazy.finishing() {
            self.err(
                format!(
                    "ICE: std module `{path}` was demanded after the passes that follow the check"
                ),
                Span::DUMMY,
            );
            return;
        }
        let Some(loads) = self.loads else {
            return;
        };
        let Some(batch) = loads.serve(path) else {
            return;
        };
        while self.loads_added <= batch {
            self.add_batch(loads.batch(self.loads_added));
            self.loads_added += 1;
        }
    }

    fn add_batch(&mut self, b: &'m Batch) {
        self.diags.extend(b.diags.iter().cloned());
        self.generic_arrow_fns.extend(b.local_fns.iter().copied());
        self.generic_arrow_all.extend(b.all_fns.iter().copied());
        self.unsafe_arrow_fns.extend(b.unsafe_fns.iter().copied());
        self.has_overloads |= b.has_overloads;
        self.overloaded_methods.extend(b.methods.iter().cloned());
        self.alias_blocked
            .extend(b.blocked.iter().map(|(k, v)| (*k, v.clone())));
        self.kept_nested.extend(b.kept.iter().copied());
        self.add_closure_groups(&b.groups);
        self.gen_throw |= b.calls_throw;
        let first = self.modules.len();
        if b.ns_private.iter().any(|p| !p.is_empty()) {
            self.ns_private.resize_with(first, Default::default);
            self.ns_private.extend(b.ns_private.iter().cloned());
        }
        for m in &b.modules {
            self.modules.push(m);
            self.scopes.push(crate::ctx::ModuleScope {
                is_std: m.is_std,
                is_library: m.is_std || m.is_dependency,
                ..Default::default()
            });
            self.import_targets.push(Default::default());
        }
        for (i, m) in b.modules.iter().enumerate() {
            for (spec, path) in &m.imports {
                if let Some(t) = crate::collect::exports::std_module(self, path) {
                    self.import_targets[first + i].entry(spec).or_insert(t);
                }
            }
        }
        for (module, spec, path) in &b.imports {
            let module = crate::collect::exports::std_module(self, module);
            let target = crate::collect::exports::std_module(self, path);
            if let (Some(m), Some(t)) = (module, target) {
                self.import_targets[m].entry(spec).or_insert(t);
            }
        }
        if !b.imports.is_empty() {
            self.export_memo.borrow_mut().changed();
        }
        let ms: Vec<usize> = (first..self.modules.len()).collect();
        let from = crate::collect::collect_added(self, &ms);
        crate::body::lazy_std::plan_added(self, &ms, from.defs);
        crate::body::check_added(self, from);
    }
}

/// What the rewrites of the added modules left to report once a `Ctx` is checked, as `check_timed`
/// does for the program's own.
pub(crate) fn report_added(cx: &mut Ctx) {
    let Some(loads) = cx.loads else {
        return;
    };
    for b in loads.batches(cx.loads_added) {
        if b.has_overloads {
            crate::overloads::report_misfits(&mut cx.diags, &b.sig_spans);
            crate::overload_adapter::check_identity(cx, &b.adapted);
        }
    }
}
