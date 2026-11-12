//! `velt dev`'s warm front end (docs/internals/design/dev-warm-front-end.md step 3): a reload of
//! an edit of function bodies alone, checked from the running version's kept check.

use std::path::{Path, PathBuf};
use std::time::Instant;

use velt_vir::vir;

use super::{
    load_stage, lower_checked, on_pipeline_thread, BuildError, BuildOptions, CheckScope, Session,
    VirFor,
};

/// A `velt dev` reload of an edit of function bodies alone (`edited`, named as sema names them),
/// from the running version's kept check (`sess.kept`, which stays there) and the source files of
/// the build that made it (`cold_files`) (dev-warm-front-end.md step 3): the program loaded and
/// parsed again, its sema from `kept` (`KeptCheck::reload`), then lowered as usual. `Ok(None)`
/// when the edit needs the cold front end; `sess.warm_refused` says why.
pub fn compile_for_jit_warm(
    sess: &mut Session,
    opts: &BuildOptions,
    cold_files: &velt_common::SourceMap,
    edited: impl FnOnce(&Session) -> Result<Vec<(PathBuf, String)>, String> + Send,
) -> Result<Option<vir::Program>, BuildError> {
    let Some(kept) = sess.kept.take() else {
        sess.warm_refused = Some("no kept check".into());
        return Ok(None);
    };
    // The kept check moves to the pipeline thread and back (it is not `Sync`); a panic there
    // loses it, and the next version is checked cold.
    let s = &mut *sess;
    let (out, kept) = on_pipeline_thread(move || {
        let out = warm_inner(s, opts, &kept, cold_files, edited);
        Ok((out, kept))
    })?;
    sess.kept = Some(kept);
    out
}

fn warm_inner(
    sess: &mut Session,
    opts: &BuildOptions,
    kept: &velt_sema::KeptCheck,
    cold_files: &velt_common::SourceMap,
    edited: impl FnOnce(&Session) -> Result<Vec<(PathBuf, String)>, String>,
) -> Result<Option<vir::Program>, BuildError> {
    {
        let scope = CheckScope {
            require_main: true,
            extra_roots: vec![],
            eager_std: false,
        };
        let loaded = load_stage(sess, opts, &scope)?;
        // The modules the cold check loaded after the program's load (the std modules it
        // demanded, the generated ones) are the kept check's: their files go into this session
        // as they were, under the same ids, which the kept check's spans name.
        let (paths, files) = (kept.module_paths(), kept.module_files());
        let same_modules = loaded.modules.len() <= paths.len()
            && (loaded.modules.iter().zip(paths.iter().zip(&files)))
                .all(|(m, (p, (f, _)))| m.path == *p && m.file == *f);
        let loaded_files = sess.sm.files().count();
        let same_files = loaded_files <= cold_files.files().count()
            && (sess.sm.files().zip(cold_files.files())).all(|((_, a), (_, b))| a.path == b.path);
        if !same_modules || !same_files {
            sess.warm_refused = Some("the program loads other modules".into());
            return Ok(None);
        }
        for (_, f) in cold_files.files().skip(loaded_files) {
            sess.sm.add(f.path.clone(), f.src.clone());
        }
        sess.std_files = (files.iter())
            .filter(|(_, std)| *std)
            .map(|(f, _)| *f)
            .collect();
        let t = Instant::now();
        // The edited functions, each named by its module's path.
        let edited = match edited(sess) {
            Ok(files) => files,
            Err(why) => {
                sess.warm_refused = Some(why);
                return Ok(None);
            }
        };
        let module_of = |file: &Path| {
            let file = std::fs::canonicalize(file).unwrap_or_else(|_| file.to_path_buf());
            (loaded.modules.iter()).find(|m| {
                let p = &sess.sm.get(m.file).path;
                std::fs::canonicalize(p).unwrap_or_else(|_| p.clone()) == file
            })
        };
        let mut named = vec![];
        for (file, name) in edited {
            let Some(m) = module_of(&file) else {
                sess.warm_refused = Some(format!("{} is not a module", file.display()));
                return Ok(None);
            };
            named.push((m.path.clone(), name));
        }
        let edited = named;
        let sema_opts = velt_sema::CheckOptions {
            require_main: scope.require_main,
            eager_std: scope.eager_std,
        };
        let checked = match kept.reload(&loaded.modules, loaded.root, &edited, sema_opts) {
            Ok(p) => *p,
            Err(why) => {
                sess.warm_refused = Some(why);
                return Ok(None);
            }
        };
        sess.record("sema (warm)", t);
        lower_checked(sess, opts, VirFor::Jit, checked).map(Some)
    }
}

/// A copy of a build's source files, under the same ids (`compile_for_jit_warm`'s `cold_files`).
pub fn copy_files(sm: &velt_common::SourceMap) -> velt_common::SourceMap {
    let mut copy = velt_common::SourceMap::new();
    for (_, f) in sm.files() {
        copy.add(f.path.clone(), f.src.clone());
    }
    copy
}
