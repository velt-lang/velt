//! The source rewrites that run before any checking context exists, in their order: each takes
//! the modules the one before produced (or the input, when it changed nothing) and gives new
//! modules when it changes any, with what it found. [`run`] runs them all; a check builds its
//! contexts over [`Rewritten::modules`] and hands the facts to them (`rewrite_facts::Found`).

use velt_common::Diagnostic;

use crate::timings;
use crate::{
    asserted_targets, call_sigs, defined_accessors, defined_props, generic_arrows, global_this,
    interface_merge, module_chunks, module_destructure, namespaces, nested_closures, overloads,
    recursive_aliases, SourceModule,
};

/// Every rewrite's result, in the order they ran.
pub(crate) struct Rewritten {
    pub chunks: module_chunks::Split,
    pub asserted: asserted_targets::Asserted,
    /// The errors of the rewrites (merging, namespaces, signatures, `globalThis`, accessors).
    pub merge_diags: Vec<Diagnostic>,
    pub destructured: Option<Vec<SourceModule>>,
    pub global_ns: namespaces::GlobalNamespaces,
    pub flat: Option<Vec<SourceModule>>,
    pub ns_private: Vec<namespaces::Private>,
    pub merged: Option<Vec<SourceModule>>,
    pub callable: Option<Vec<SourceModule>>,
    pub lifted: Option<generic_arrows::Lifted>,
    pub overloaded: Option<overloads::Rewritten>,
    pub rewritten: Option<Vec<SourceModule>>,
    pub alias_blocked: recursive_aliases::Blocked,
    pub global_props: global_this::GlobalProps,
    pub globals: Option<Vec<SourceModule>>,
    pub accessors: Option<Vec<SourceModule>>,
    pub brands: defined_props::Brands,
    pub branded: Option<Vec<SourceModule>>,
    pub closures: Option<nested_closures::Rewritten>,
}

impl Rewritten {
    /// The modules the last rewrite that changed any produced, else `input`.
    pub(crate) fn modules<'a>(&'a self, input: &'a [SourceModule]) -> &'a [SourceModule] {
        (self.closures.as_ref().map(|c| &c.modules[..]))
            .or(self.branded.as_deref())
            .or(self.accessors.as_deref())
            .or(self.globals.as_deref())
            .or(self.rewritten.as_deref())
            .or(self.overloaded.as_ref().map(|o| &o.modules[..]))
            .or(self.lifted.as_ref().map(|l| &l.modules[..]))
            .or(self.callable.as_deref())
            .or(self.merged.as_deref())
            .or(self.flat.as_deref())
            .or(self.destructured.as_deref())
            .or(self.asserted.modules.as_deref())
            .or(self.chunks.modules.as_deref())
            .unwrap_or(input)
    }
}

/// Run every rewrite over `modules` (whose root is `root`).
pub(crate) fn run(modules: &[SourceModule], root: usize) -> Rewritten {
    let chunks = timings::pass("rewrite: module_chunks", || {
        module_chunks::split(modules, root)
    });
    let modules = chunks.modules.as_deref().unwrap_or(modules);
    let asserted = timings::pass("rewrite: asserted_targets", || {
        asserted_targets::rewrite(modules)
    });
    let modules = asserted.modules.as_deref().unwrap_or(modules);
    let mut merge_diags = chunks.diags.clone();
    merge_diags.extend(timings::pass("rewrite: call_sigs::check", || {
        call_sigs::check(modules)
    }));
    let destructured = timings::pass("rewrite: module_destructure", || {
        module_destructure::rewrite(modules)
    });
    let modules = destructured.as_deref().unwrap_or(modules);
    let global_ns = timings::pass("rewrite: global_namespaces", || {
        namespaces::global_namespaces(modules)
    });
    let (flat, ns_private, diags) = timings::pass("rewrite: namespaces::flatten", || {
        namespaces::flatten(modules)
    });
    merge_diags.extend(diags);
    let modules = flat.as_deref().unwrap_or(modules);
    let (merged, diags) = timings::pass("rewrite: interface_merge", || {
        interface_merge::rewrite(modules)
    });
    merge_diags.extend(diags);
    let modules = merged.as_deref().unwrap_or(modules);
    let (callable, diags) = timings::pass("rewrite: call_sigs", || call_sigs::rewrite(modules));
    merge_diags.extend(diags);
    let modules = callable.as_deref().unwrap_or(modules);
    let lifted = timings::pass("rewrite: generic_arrows", || generic_arrows::lift(modules));
    let modules = lifted.as_ref().map_or(modules, |l| &l.modules[..]);
    let overloaded = timings::pass("rewrite: overloads", || overloads::rewrite(modules));
    let modules = overloaded.as_ref().map_or(modules, |o| &o.modules[..]);
    let (rewritten, alias_blocked) = timings::pass("rewrite: recursive_aliases", || {
        recursive_aliases::rewrite(modules)
    });
    let modules = rewritten.as_deref().unwrap_or(modules);
    let (global_props, global_diags, globals) =
        timings::pass("rewrite: global_this", || global_this::rewrite(modules));
    merge_diags.extend(global_diags);
    let modules = globals.as_deref().unwrap_or(modules);
    let (accessor_diags, accessors) = timings::pass("rewrite: defined_accessors", || {
        defined_accessors::rewrite(modules)
    });
    merge_diags.extend(accessor_diags);
    let modules = accessors.as_deref().unwrap_or(modules);
    let (brands, branded) =
        timings::pass("rewrite: defined_props", || defined_props::rewrite(modules));
    let modules = branded.as_deref().unwrap_or(modules);
    let closures = timings::pass("rewrite: nested_closures", || {
        nested_closures::rewrite(modules)
    });
    Rewritten {
        chunks,
        asserted,
        merge_diags,
        destructured,
        global_ns,
        flat,
        ns_private,
        merged,
        callable,
        lifted,
        overloaded,
        rewritten,
        alias_blocked,
        global_props,
        globals,
        accessors,
        brands,
        branded,
        closures,
    }
}
