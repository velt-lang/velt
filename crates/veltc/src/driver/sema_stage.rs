//! The type check of a loaded program ([`sema_stage`]), checked again with the std modules it
//! demanded that could not be served in the middle of it loaded up front, and the native
//! declarations checked against their libraries.

use velt_loader as loader;
use velt_sema::hir;

use super::{
    load_modules, native_executor, prebuilt_finder, std_cache, std_summary, units_key, BuildError,
    BuildOptions, CheckScope, Session, StdLoad,
};

/// Type-check the loaded program (and the native declarations against their libraries).
///
/// A std module the check demands that can't be added in the middle of it (velt_loader's (a1)
/// rule: the TLS client and server, docs/internals/design/lazy-std.md "Loading on demand") is
/// refused there; the check is then thrown away and the program loaded again with that module up
/// front ([`CheckScope::preload`]) and checked again, once per such demand. So a module that is
/// not served in place still loads only in the programs whose live code reaches it.
pub(super) fn sema_stage(
    sess: &mut Session,
    opts: &BuildOptions,
    scope: &CheckScope,
    loaded: &mut loader::Loaded,
) -> Result<hir::Program, BuildError> {
    let mut scope = std::borrow::Cow::Borrowed(scope);
    // The files the session had before the program's load, which a load again keeps.
    let before_load = (loaded.modules.iter()).map(|m| m.file.0).min().unwrap_or(0);
    loop {
        let diags_before = sess.diagnostics.len();
        let (hir, refused) = sema_once(sess, opts, &scope, loaded);
        let fresh: Vec<String> = (refused.into_iter())
            .filter(|p| !scope.preload.contains(p))
            .collect();
        if fresh.is_empty() {
            return finish_sema(sess, opts, hir);
        }
        // What the thrown-away check reported, or kept, is not the program's.
        sess.diagnostics.truncate(diags_before);
        sess.pending_kept = None;
        scope.to_mut().preload.extend(fresh);
        // The load again numbers its files as the first one did: what the check makes without a
        // file of its own (`Span::DUMMY`, file 0) stays in the first std module.
        let mut sm = velt_common::SourceMap::new();
        for (_, f) in sess.sm.files().take(before_load as usize) {
            sm.add(f.path.clone(), f.src.clone());
        }
        sess.sm = sm;
        *loaded = load_modules(sess, opts, &scope)?;
        sess.stop_if_errors()?;
    }
}

/// One check of [`sema_stage`]: the program, and the std modules it demanded that could not be
/// served in place.
fn sema_once(
    sess: &mut Session,
    opts: &BuildOptions,
    scope: &CheckScope,
    loaded: &mut loader::Loaded,
) -> (Option<hir::Program>, Vec<String>) {
    let sema_opts = velt_sema::CheckOptions {
        require_main: scope.require_main,
        eager_std: scope.eager_std,
        units: scope.units,
        std_key: units_key::units_std_key(sess, scope, loaded),
        native_executor: native_executor(opts),
        wasm32: super::wasm32(opts),
        jit: scope.jit,
    };
    let packages = opts.packages.as_ref();
    sess.no_generators |= opts.no_generators;
    // With `VELT_EAGER_STD` the executor's modules are still served on demand
    // (`StdLoad::serve`); `velt check --std` loads everything up front.
    let demands = !scope.eager_std;
    let generator_build = sess.generator_build;
    let finder = prebuilt_finder(sess, opts, scope);
    let mut more = demands.then(|| {
        StdLoad::new(
            (&mut loaded.state, &loaded.modules),
            generator_build,
            (opts, scope),
            finder.as_ref(),
        )
    });
    let hir = crate::generate::check(
        sess,
        &loaded.modules,
        loaded.root,
        sema_opts,
        packages,
        more.as_mut(),
    );
    let refused = more
        .as_mut()
        .map_or(vec![], |m| std::mem::take(&mut m.refused));
    sess.std_files = StdLoad::std_files(more, &loaded.modules);
    if let Some(finder) = finder {
        sess.prebuilt = std_cache::prebuilt_found(finder, &mut sess.generator_inputs);
    }
    sess.native = crate::native::check_trust(
        &loaded.modules,
        &sess.sm,
        opts.packages.as_ref(),
        &opts.target(),
        &mut sess.diagnostics,
    );
    (hir, refused)
}

/// The rest of [`sema_stage`] after the check that stands: the native declarations against their
/// libraries.
fn finish_sema(
    sess: &mut Session,
    opts: &BuildOptions,
    mut hir: Option<hir::Program>,
) -> Result<hir::Program, BuildError> {
    if let Some(hir) = &mut hir {
        let std_root = loader::std_root();
        let std_files = sess.std_files.iter().copied().collect();
        sess.c_constants = crate::native::read_c_constants(
            hir,
            &sess.sm,
            opts.packages.as_ref(),
            &std_files,
            &opts.target(),
            &mut sess.generator_inputs,
            &mut sess.diagnostics,
        );
        sess.c_checked = crate::native::check_declares(
            hir,
            &sess.sm,
            std_root.as_deref().filter(|_| opts.std_c_headers),
            std_root.as_deref(),
            opts.packages.as_ref(),
            &std_files,
            &opts.target(),
            &mut sess.diagnostics,
        );
        crate::native::note_checked(&mut sess.native, opts.packages.as_ref(), &sess.c_checked);
        crate::native::c_stamp_inputs(&sess.c_checked, &mut sess.generator_inputs);
        sess.c_libs = crate::native::c_links(hir, &sess.sm, opts.packages.as_ref(), &opts.target());
        std_summary::write(hir, &opts.input.display().to_string());
    }
    sess.stop_if_errors()?;
    hir.ok_or_else(|| BuildError::Ice("sema returned no program but reported no errors".into()))
}
