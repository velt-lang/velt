//! C libraries in the `velt dev` JIT host (docs/internals/design/c-ffi.md "Where libraries come
//! from"): the system libraries and those packages build from `sources`, loaded into the host
//! (nothing is linked); a change to a package's C sources restarts the host instead of swapping
//! ([`Fingerprint`]).

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use vpm::PackageGraph;

/// Open the C libraries a version of the program calls (`c_libs`, a session's): system libraries,
/// and those packages build from `sources`, built first if a source changed.
pub fn load_c_libraries(
    opts: &crate::driver::BuildOptions,
    c_libs: &[String],
) -> Result<(), String> {
    velt_codegen_cl::load_c_libraries(&crate::native::c_system_libraries(c_libs))?;
    let files = crate::native::c_dev_libraries(opts.packages.as_ref(), c_libs)?;
    velt_codegen_cl::load_c_library_files(&files)
}

/// When the C sources of the libraries built here were last changed: a different fingerprint
/// after a rebuild means the running host has stale native code.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Fingerprint(Vec<(PathBuf, Option<SystemTime>)>);

impl Fingerprint {
    /// The fingerprint of `graph`'s C libraries built from source.
    pub fn of(graph: Option<&PackageGraph>) -> Fingerprint {
        Fingerprint(
            source_files(graph)
                .into_iter()
                .map(|f| {
                    let t = std::fs::metadata(&f).and_then(|m| m.modified()).ok();
                    (f, t)
                })
                .collect(),
        )
    }
}

/// The C sources of the libraries packages build here: the files `velt dev` watches besides the
/// Velt sources.
pub fn source_files(graph: Option<&PackageGraph>) -> Vec<PathBuf> {
    let mut out = vec![];
    // A package's C sources: all of `native/` is what they are built from.
    for pkg in graph.into_iter().flat_map(|g| g.packages()) {
        if matches!(pkg.c_bundle, Some(vpm::graph::CBundle::FromSource(_))) {
            collect(&pkg.root.join(vpm::manifest::C_SOURCES_DIR), &mut out);
        }
    }
    out.sort();
    out
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let path = e.path();
        let name = e.file_name();
        if path.is_dir() {
            if name != "target" && name != ".git" {
                collect(&path, out);
            }
        } else {
            out.push(path);
        }
    }
}
