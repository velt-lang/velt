//! Loading in the middle of a check (#2161 (a1), docs/internals/design/lazy-std.md "Loading on
//! demand"): [`load_more`] goes on from a finished [`load_program`](crate::load_program) with the
//! std modules sema demands, and what they import that is not loaded yet. Modules it serves are
//! appended in demand order, each before its imports, and follow the (a1) rule
//! ([`crate::std_index`]): a module that breaks it is an internal error, since the check cannot
//! take it in place. The rule is applied to the parses at hand, so no index entry is read.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use velt_common::{Diagnostic, Diagnostics, SourceMap, Span};
use velt_sema::{SourceModule, StdBatch};
use velt_syntax::ast;

use crate::{
    ambient, case, conditional, locate::Origin, paths::file_key, std_ast, std_index, wanted,
};
use crate::{LoadOptions, Loader};

/// What a load keeps for [`load_more`]. Moving it out of the load costs nothing; the modules'
/// paths and imports are copied only when a demand comes.
#[derive(Default)]
pub struct LoadState {
    /// Per module: its path, file and imports; a module [`load_more`] served also has its import
    /// items (those it may have to resolve again), which its [`base`](Self::base) ones have in
    /// the caller's modules.
    pub(crate) modules: Vec<SourceModule>,
    /// How many of `modules` are the caller's.
    pub(crate) base: usize,
    pub(crate) origins: Vec<(PathBuf, Origin)>,
    pub(crate) by_file: HashMap<PathBuf, usize>,
    pub(crate) dir_names: case::DirNames,
    pub(crate) npm: vpm::npm::NpmResolver,
    pub(crate) wanted: wanted::WantedByModule,
    pub(crate) resolved: HashSet<usize>,
    pub(crate) effects: HashMap<PathBuf, bool>,
    pub(crate) skipped: HashMap<(usize, String), Option<(PathBuf, PathBuf)>>,
    pub(crate) harness: Option<usize>,
    pub(crate) std_imports: Option<vpm::imports::ImportMap>,
    /// The old-extension imports already warned about, so a module loaded later doesn't warn again.
    pub(crate) warned_old_extension: HashSet<String>,
}

impl<'a, 'o> Loader<'a, 'o> {
    pub(crate) fn new(
        sm: &'a mut SourceMap,
        diags: &'a mut Diagnostics,
        opts: &'a LoadOptions<'o>,
        overlay: HashMap<PathBuf, String>,
        state: LoadState,
    ) -> Self {
        Loader {
            base: &[],
            sm,
            diags,
            opts,
            std_parser: std_ast::StdParser::new(opts.std_ast_cache),
            overlay,
            modules: state.modules,
            origins: state.origins,
            by_file: state.by_file,
            std_key: opts.std_root.as_deref().map(file_key),
            extra_phase: false,
            dir_names: state.dir_names,
            globals: vec![],
            said: Default::default(),
            warned_old_extension: state.warned_old_extension,
            npm: state.npm,
            ambient: ambient::Ambients::new(opts.packages),
            global_vars: vec![],
            wanted: state.wanted,
            resolved: state.resolved,
            effects: state.effects,
            skipped: state.skipped,
            harness: state.harness,
            conditions: conditional::load_conditions(opts),
            std_imports: state.std_imports,
            variant_checks: vec![],
            variant_tables: HashMap::new(),
            variant_sigs: HashMap::new(),
            variant_stars: HashMap::new(),
            variant_busy: Default::default(),
            variant_star_busy: Default::default(),
            variant_cut: false,
        }
    }

    /// What [`load_more`] goes on from (`modules` is the caller's by now).
    pub(crate) fn into_state(self) -> LoadState {
        LoadState {
            modules: self.modules,
            base: 0,
            origins: self.origins,
            by_file: self.by_file,
            dir_names: self.dir_names,
            npm: self.npm,
            wanted: self.wanted,
            resolved: self.resolved,
            effects: self.effects,
            skipped: self.skipped,
            harness: self.harness,
            std_imports: self.std_imports,
            warned_old_extension: self.warned_old_extension,
        }
    }
}

/// Do the std modules a check may demand load during it? Not with `VELT_EAGER_STD`, which loads
/// them up front to compare (docs/internals/design/lazy-std.md "Loading on demand"), or with
/// `VELT_CHECK_ALL_STD`, which checks every definition of what is loaded.
pub fn demand_std_on() -> bool {
    std::env::var_os("VELT_EAGER_STD").is_none() && std::env::var_os("VELT_CHECK_ALL_STD").is_none()
}

/// `m` without its syntax, but for the items `resolve_imports` reads when `imports` is set.
fn skeleton(m: &SourceModule, imports: bool) -> SourceModule {
    let items = match imports {
        false => vec![],
        true => (m.ast.items.iter())
            .filter(|i| matches!(i.kind, ast::ItemKind::Import(_) | ast::ItemKind::Augment(_)))
            .cloned()
            .collect(),
    };
    SourceModule {
        path: m.path.clone(),
        is_std: m.is_std,
        is_dependency: m.is_dependency,
        file: m.file,
        ast: ast::Module {
            items,
            span: m.ast.span,
            jsx_import_source: m.ast.jsx_import_source.clone(),
            fn_names: m.ast.fn_names.clone(),
        },
        imports: m.imports.clone(),
        jsx_runtime: m.jsx_runtime.clone(),
    }
}

/// The specifier that names std module `path` (canonical) from inside std.
fn specifier(path: &str) -> Option<String> {
    let rel = path.strip_prefix(velt_common::std_modules::CANONICAL_PREFIX)?;
    Some(format!(
        "{}{rel}",
        velt_common::std_modules::SPECIFIER_PREFIX
    ))
}

/// Load the std modules `demanded` (canonical paths) and what they import that is not loaded: the
/// modules, new to this load, in order. `base` is the modules of the load `state` came from (a
/// longer list that starts with them does too). `opts` are the load's.
pub fn load_more(
    sm: &mut SourceMap,
    state: &mut LoadState,
    base: &[SourceModule],
    demanded: &[String],
    opts: &LoadOptions,
    diags: &mut Diagnostics,
) -> StdBatch {
    // As `load_up_front`: a std root (a test's, `VELT_STD`'s) may not have what is demanded, or be
    // there at all. The module stays missing, and the hook's lookup finds nothing.
    let in_std_root = |path: &&String| {
        specifier(path).is_none() || opts.std_root.as_deref().is_some_and(|s| exists(s, path))
    };
    let demanded: Vec<&String> = demanded.iter().filter(in_std_root).collect();
    if demanded.is_empty() || !base.iter().any(|m| m.is_std) {
        return StdBatch::default();
    }
    let mut taken = std::mem::take(state);
    if taken.modules.is_empty() {
        taken.modules = base.iter().map(|m| skeleton(m, false)).collect();
        taken.base = base.len();
    }
    let first_base = taken.base;
    let overlay = (opts.overlay.iter().flat_map(|o| o.iter()))
        .map(|(p, s)| (file_key(p), s.clone()))
        .collect();
    let mut loader = Loader::new(sm, diags, opts, overlay, taken);
    loader.base = &base[..first_base];
    let start = loader.modules.len();
    let imports_before: Vec<usize> = loader.modules.iter().map(|m| m.imports.len()).collect();
    let mut queue = VecDeque::new();
    if let Some(importer) = (loader.modules.iter()).position(|m| m.is_std) {
        for path in demanded {
            match specifier(path) {
                // The compiler's import, not the source's: no visibility rule.
                Some(spec) => {
                    loader.import(importer, &spec, Span::DUMMY, false, &mut queue);
                }
                None => loader.diags.push(Diagnostic::error(
                    format!("ICE: `{path}` was demanded, and it is no std module"),
                    Span::DUMMY,
                )),
            }
        }
        loader.resolve_all(&mut queue);
    }
    let mut out = StdBatch::default();
    for (i, before) in imports_before.iter().enumerate() {
        let m = &loader.modules[i];
        for (spec, target) in &m.imports[*before..] {
            out.imports
                .push((m.path.clone(), spec.clone(), target.clone()));
        }
    }
    for m in &mut loader.modules[start..] {
        let kept = skeleton(m, true);
        for why in std_index::violations(&m.ast) {
            if opts.refuse_out_of_rule {
                out.refused.push(format!("`{}` {why}", m.path));
                continue;
            }
            loader.diags.push(Diagnostic::error(
                format!(
                    "ICE: std module `{}` was demanded during the check, and it cannot be served in place: {why}",
                    m.path
                ),
                Span::DUMMY,
            ));
        }
        out.modules.push(std::mem::replace(m, kept));
    }
    *state = loader.into_state();
    state.base = first_base;
    out
}

/// [`LoadOptions::preload`]: those std modules, after everything else (a std root without one
/// leaves it out, as a demand would find it missing).
pub(crate) fn preload(loader: &mut Loader, queue: &mut VecDeque<usize>) {
    let Some(std) = loader.opts.std_root.clone() else {
        return;
    };
    let Some(importer) = loader.modules.iter().position(|m| m.is_std) else {
        return;
    };
    for path in loader.opts.preload {
        let Some(spec) = specifier(path) else {
            continue;
        };
        if exists(&std, path) {
            loader.import(importer, &spec, Span::DUMMY, false, queue);
        }
    }
    loader.resolve_all(queue);
}

/// Without [`LoadOptions::demand_std`]: every module a demand could ask for, after everything
/// else, in table order, and the executor in Velt when it is selected (sema demands it for a
/// program that runs something on it, #2649 P3(a)). A test's own std root may not have them.
pub(crate) fn load_up_front(loader: &mut Loader, queue: &mut VecDeque<usize>) {
    let Some(std) = loader.opts.std_root.clone() else {
        return;
    };
    let Some(importer) = loader.modules.iter().position(|m| m.is_std) else {
        return;
    };
    // The executor in Velt, on a native target (the `std-scheduler` condition); with it, what std
    // calls by role (`hookCall`, #2882).
    let conditions = super::conditional::load_conditions(loader.opts);
    let native = conditions.iter().any(|c| c == vpm::imports::STD_SCHEDULER);
    // std's writer of stdout and stderr is a native target's (`#stdout-sys`' `native` variant
    // calls it): a WebAssembly build's writer is the runtime's, and the writer's `@export` has no
    // WebAssembly form.
    let native_target = conditions.iter().any(|c| c == "native");
    let writer = velt_common::std_module!(lang_hooks_out_native);
    // A debug compiler's cell check under it too (velt_sema `lazy_std` `sched_hooks`); none
    // when the caller serves them on demand still (`LoadOptions::demand_executor`).
    let cells = velt_common::std_module!(lang_hooks_sched_cells);
    let sched = (EXECUTOR_MODULES.iter().copied())
        .filter(|_| native && !loader.opts.demand_executor)
        .filter(|m| *m != cells || cfg!(debug_assertions));
    let paths = (velt_common::std_hooks::ON_DEMAND_MODULES.iter().copied())
        .filter(|p| native_target || *p != writer);
    for path in paths.chain(sched) {
        let Some(spec) = specifier(path) else {
            continue;
        };
        if !exists(&std, path) {
            continue;
        }
        loader.import(importer, &spec, Span::DUMMY, false, queue);
    }
    loader.resolve_all(queue);
}

/// The executor in Velt's modules sema demands as a program needs them: its hooks, a debug
/// compiler's cell check, and what std calls by role (#2882): its blocking pool, its reactor, its
/// child processes and its resolver.
const EXECUTOR_MODULES: &[&str] = &[
    velt_common::std_module!(lang_hooks_sched),
    velt_common::std_module!(lang_hooks_program_thread),
    velt_common::std_module!(lang_hooks_sched_spawn),
    velt_common::std_module!(lang_hooks_sched_blocking),
    velt_common::std_module!(lang_hooks_sched_cells),
    velt_common::std_module!(lang_hooks_sched_io),
    velt_common::std_module!(process_child_reactor),
    velt_common::std_module!(net_dns_lookup_sched),
];

/// The modules of the TLS client and server, which `hookCall`s of `sched_tls_listen`,
/// `sched_tls_connect` and `sched_tls_handshake` demand (velt_common `std_hooks.rs`, #2392).
/// Not loaded up front with the rest (`load_up_front`): a program that can't connect or listen
/// with TLS never has them, and one that can has them loaded for the check again that a refused
/// demand of them starts (`LoadOptions::refuse_out_of_rule`, `preload`).
const TLS_MODULES: &[&str] = &[
    velt_common::std_module!(net_tls_fd_client),
    velt_common::std_module!(net_tls_fd_server),
];

/// Is `path` (a canonical std module path) the TLS client's or server's module, which a caller
/// with std up front (`VELT_EAGER_STD`) still lets a check demand, as without it?
pub fn tls_module(path: &str) -> bool {
    TLS_MODULES.contains(&path)
}

/// The module of `JSON.stringify(value, replacer)` with a replacer function, which the prelude's
/// `jsonStringifyReplaced` demands with a `hookCall` of `json_stringify_replaced` (#3774). Not
/// loaded up front with the rest (`load_up_front`): it imports the prelude's JSON modules, which
/// only a program that names `JSON` loads, so a program without a replacer function has none of
/// it either way.
const JSON_REPLACE_MODULES: &[&str] = &[velt_common::std_module!(lang_hooks_json_replace)];

/// Is `path` (a canonical std module path) the module of `JSON.stringify`'s replacer function,
/// which a caller with std up front (`VELT_EAGER_STD`) still lets a check demand, as without it?
pub fn json_replace_module(path: &str) -> bool {
    JSON_REPLACE_MODULES.contains(&path)
}

/// Is `path` (a canonical std module path) one of the executor in Velt's modules, which a caller
/// with std up front may still serve on demand (`LoadOptions::demand_executor`)?
pub fn executor_module(path: &str) -> bool {
    EXECUTOR_MODULES.contains(&path)
}

/// Does std root `std` hold module `path` as a file?
fn exists(std: &Path, path: &str) -> bool {
    let rel = path
        .strip_prefix(velt_common::std_modules::CANONICAL_PREFIX)
        .unwrap_or(path);
    vpm::probe::is_file(&std.join(format!("{rel}.velt")))
}

#[cfg(test)]
#[path = "more_tests.rs"]
mod tests;
