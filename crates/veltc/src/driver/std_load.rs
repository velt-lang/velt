//! Loading what sema demands during a check.

use velt_loader::{self as loader, LoadOptions};

use super::{BuildOptions, CheckScope, Session};

/// How the program is loaded, by [`load_modules`] and when sema demands more ([`StdLoad`]).
pub(super) fn load_options<'a>(
    generator_build: bool,
    opts: &'a BuildOptions,
    scope: &'a CheckScope,
) -> LoadOptions<'a> {
    LoadOptions {
        std_root: loader::std_root(),
        packages: opts
            .packages
            .as_ref()
            .map(|g| g as &dyn loader::PackageResolver),
        root_source: opts.root_source.clone(),
        test_harness: opts.test_harness,
        overlay: None,
        // A source generator host for std's generators sits in the std root and imports their
        // internal files: it is std code (docs/internals/design/source-generators.md §5).
        std_root_as_std: generator_build,
        target: Some(opts.target()),
        host_condition: opts.host.as_ref().map(|h| h.config.condition.clone()),
        check_variants: opts.check_variants,
        extra_roots: &scope.extra_roots,
        std_ast_cache: crate::build_cache::std_ast_cache(),
        demand_std: !scope.eager_std && loader::demand_std_on(),
        // `VELT_EAGER_STD`: all of std up front but the executor's modules, which are served on
        // demand still (`StdLoad::serve`). `velt check --std` and `VELT_CHECK_ALL_STD`, which check
        // with laziness off (no live set demands the executor), load them up front too.
        demand_executor: !scope.eager_std && std::env::var_os("VELT_CHECK_ALL_STD").is_none(),
        preload: &scope.preload,
        refuse_out_of_rule: true,
    }
}

/// How sema loads what it demands during a check (`velt_sema::std_load`): [`loader::load_more`]
/// going on from the program's load.
pub(crate) struct StdLoad<'a> {
    state: &'a mut loader::LoadState,
    base: &'a [velt_sema::SourceModule],
    generator_build: bool,
    opts: &'a BuildOptions,
    scope: &'a CheckScope,
    /// The files of the modules served.
    pub(super) files: Vec<velt_common::FileId>,
    /// The modules served, which the check owns from then on ([`Served`]).
    pub(crate) served: Vec<Served>,
    /// The demanded modules that could not be served in place (velt_loader
    /// `LoadOptions::refuse_out_of_rule`): the driver checks the program again with them loaded
    /// up front (`sema_stage`).
    pub(super) refused: Vec<String>,
    /// The prebuilt hook modules' objects the build may link instead of loading the modules
    /// (`std_cache::prebuilt`, `velt_sema::Loads::prebuilt`).
    pub(crate) prebuilt: Option<&'a super::std_cache::Finder>,
}

impl<'a> StdLoad<'a> {
    pub(super) fn new(
        (state, base): (&'a mut loader::LoadState, &'a [velt_sema::SourceModule]),
        generator_build: bool,
        (opts, scope): (&'a BuildOptions, &'a CheckScope),
        prebuilt: Option<&'a super::std_cache::Finder>,
    ) -> Self {
        StdLoad {
            state,
            base,
            generator_build,
            opts,
            scope,
            files: vec![],
            served: vec![],
            refused: vec![],
            prebuilt,
        }
    }

    /// The files of the std modules in `modules` and of those `more` served
    /// ([`Session::std_files`]).
    pub(super) fn std_files(
        more: Option<Self>,
        modules: &[velt_sema::SourceModule],
    ) -> Vec<velt_common::FileId> {
        let served = more.map_or(vec![], |m| m.files);
        let std = modules.iter().filter(|m| m.is_std).map(|m| m.file);
        std.chain(served).collect()
    }

    pub(crate) fn serve(&mut self, sess: &mut Session, demanded: &[String]) -> velt_sema::StdBatch {
        // With std up front (`VELT_EAGER_STD`), only the executor's modules and the TLS client's
        // and server's are left to demand (the TLS ones are refused, so the program is checked
        // again with them up front, as without it); anything else demanded is not served, as
        // without a loader.
        let executor_only: Vec<String>;
        let demanded = match loader::demand_std_on() {
            true => demanded,
            false => {
                executor_only = (demanded.iter())
                    .filter(|p| loader::executor_module(p) || loader::tls_module(p))
                    .cloned()
                    .collect();
                &executor_only[..]
            }
        };
        // After a refusal this check is thrown away: what it demands next is not served (the
        // load's state holds the refused modules, which the check does not).
        if !self.refused.is_empty() {
            self.refused.extend(demanded.iter().cloned());
            return refusal(demanded);
        }
        let load = load_options(self.generator_build, self.opts, self.scope);
        let (sm, diags) = (&mut sess.sm, &mut sess.diagnostics);
        let batch = loader::load_more(sm, self.state, self.base, demanded, &load, diags);
        if !batch.refused.is_empty() {
            self.refused.extend(demanded.iter().cloned());
            return refusal(demanded);
        }
        self.files.extend(batch.modules.iter().map(|m| m.file));
        self.served.extend(batch.modules.iter().map(|m| Served {
            path: m.path.clone(),
            file: m.file,
            imports: m.imports.clone(),
        }));
        batch
    }
}

/// A std module served on demand, as the driver still knows it once the check owns it: what
/// generating for a generated function it declares needs (`crate::generate`), since the
/// program's load does not hold the module.
pub(crate) struct Served {
    pub path: String,
    pub file: velt_common::FileId,
    /// [`velt_sema::SourceModule::imports`].
    pub imports: Vec<(String, String)>,
}

/// The answer to a refused demand: it tells the check to stop (velt_sema `StdLoads::refused`).
fn refusal(demanded: &[String]) -> velt_sema::StdBatch {
    velt_sema::StdBatch {
        refused: demanded.to_vec(),
        ..Default::default()
    }
}
