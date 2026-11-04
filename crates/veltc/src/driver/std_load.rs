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
}

impl<'a> StdLoad<'a> {
    pub(super) fn new(
        (state, base): (&'a mut loader::LoadState, &'a [velt_sema::SourceModule]),
        generator_build: bool,
        opts: &'a BuildOptions,
        scope: &'a CheckScope,
    ) -> Self {
        StdLoad {
            state,
            base,
            generator_build,
            opts,
            scope,
            files: vec![],
        }
    }

    pub(crate) fn serve(&mut self, sess: &mut Session, demanded: &[String]) -> velt_sema::StdBatch {
        let load = load_options(self.generator_build, self.opts, self.scope);
        let (sm, diags) = (&mut sess.sm, &mut sess.diagnostics);
        let batch = loader::load_more(sm, self.state, self.base, demanded, &load, diags);
        self.files.extend(batch.modules.iter().map(|m| m.file));
        batch
    }
}
