//! The C libraries packages build from `sources` in a program (docs/internals/design/c-ffi.md
//! "Where libraries come from"): a program's declarations name them (`bundled:<package>/<library>`
//! among its system libraries, which a build's stamp keeps), and the link or the dev host asks here
//! for their files. A package built here has them built now, by its bundle's key: a changed source
//! rebuilds, whether or not the program's own files changed, and an unchanged one costs a hash.

use std::path::PathBuf;

use vpm::graph::{CBundle, GraphPackage};
use vpm::native::c_build::{self, CBuildRequest};
use vpm::native::{CLibraryFiles, NativeLib, NativeOrigin};
use vpm::{Manifest, PackageGraph};

const BUNDLED: &str = "bundled:";

/// What [`super::c_links`] holds for a C library of `package` built from `sources`.
pub(super) fn entry(package: &str, library: &str) -> String {
    format!("{BUNDLED}{package}/{library}")
}

/// The entries of `c_libs` that name a system library.
pub fn system_libraries(c_libs: &[String]) -> Vec<String> {
    (c_libs.iter())
        .filter(|l| !l.starts_with(BUNDLED))
        .cloned()
        .collect()
}

/// The bundle of `package` for `target`: published, or built here.
fn bundle_of(package: &GraphPackage, target: &str) -> Result<NativeLib, String> {
    match &package.c_bundle {
        Some(CBundle::Prebuilt(lib)) => Ok((**lib).clone()),
        Some(CBundle::FromSource(out)) => {
            let manifest = Manifest::from_dir(&package.root)?;
            c_build::build(CBuildRequest {
                root: &package.root,
                manifest: &manifest,
                target,
                out,
                origin: NativeOrigin::BuiltFromSource,
            })
        }
        None => Err(format!(
            "package `{}` has no bundle of its C libraries for {target}",
            package.name
        )),
    }
}

/// The libraries of `c_libs` built from `sources`: each with its bundle's directory and files.
fn bundled(
    graph: Option<&PackageGraph>,
    c_libs: &[String],
    target: &str,
) -> Result<Vec<(PathBuf, CLibraryFiles)>, String> {
    let mut out = Vec::new();
    // Each package's bundle once, with all its libraries, however many of them are named.
    let mut bundles: Vec<(&str, NativeLib)> = Vec::new();
    for named in c_libs.iter().filter_map(|l| l.strip_prefix(BUNDLED)) {
        let Some((package, library)) = named.rsplit_once('/') else {
            return Err(format!("ICE: `{named}` is no bundled C library"));
        };
        let found = graph.and_then(|g| g.packages().iter().find(|p| p.name == package));
        let Some(found) = found else {
            return Err(format!(
                "package `{package}`, whose C library `{library}` the program calls, is not in its dependencies"
            ));
        };
        if !bundles.iter().any(|(p, _)| *p == package) {
            bundles.push((package, bundle_of(found, target)?));
        }
        let lib = bundles
            .iter()
            .find(|(p, _)| *p == package)
            .map(|(_, lib)| lib)
            .ok_or("ICE: a bundle just added")?;
        let files =
            lib.meta.c.get(library).ok_or_else(|| {
                format!("package `{package}`'s bundle has no C library `{library}`")
            })?;
        out.push((lib.dir.clone(), files.clone()));
    }
    Ok(out)
}

/// What the linker adds for the libraries of `c_libs` built from `sources`: the shared library in
/// debug builds, and the static one in release builds so the executable stays self-contained
/// (where there is one: not on Windows, as for a crate). A musl program is static: always that.
pub fn links(
    graph: Option<&PackageGraph>,
    c_libs: &[String],
    release: bool,
    target: &str,
) -> Result<Vec<velt_link::NativeLink>, String> {
    let links = bundled(graph, c_libs, target)?.into_iter().map(|(dir, f)| {
        let wanted = release || target.contains("-musl");
        velt_link::NativeLink {
            shared: dir.join(&f.shared),
            import_lib: f.import_lib.map(|p| dir.join(p)),
            static_obj: f.static_lib.filter(|_| wanted).map(|p| dir.join(p)),
            system_libs: Vec::new(),
        }
    });
    Ok(links.collect())
}

/// The shared libraries `velt dev` loads for `c_libs`, on this machine.
pub fn dev_libraries(
    graph: Option<&PackageGraph>,
    c_libs: &[String],
) -> Result<Vec<PathBuf>, String> {
    let host = velt_codegen_cl::host_triple();
    let found = bundled(graph, c_libs, &host)?;
    Ok(found.into_iter().map(|(d, f)| d.join(f.shared)).collect())
}
