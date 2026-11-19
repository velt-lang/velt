//! What a build is asked for: the options of one compilation, and the host package of a
//! `--host` build.

use std::path::PathBuf;

use crate::backend::Backend;
use crate::cli::Emit;

/// What to build and how.
#[derive(Clone, Debug, Default)]
pub struct BuildOptions {
    /// Root module file (with `root_source`: the virtual path relative imports resolve from).
    pub input: PathBuf,
    /// Root module source instead of reading `input` (generated `velt test` harnesses).
    pub root_source: Option<String>,
    /// The root is `velt test`'s generated harness, which imports the test runner's hooks from
    /// velt/test's internal files ([`velt_loader::LoadOptions::test_harness`]).
    pub test_harness: bool,
    /// Installed packages when building inside a package (`None` → package imports fail).
    pub packages: Option<vpm::PackageGraph>,
    /// `-o`; `None` → `./target/velt/<stem>[.exe]`.
    pub output: Option<PathBuf>,
    /// Optimize and link without debug info.
    pub release: bool,
    /// `-g`: keep source locations for debug info in a release build (debug builds always do).
    pub debug_info: bool,
    /// Target triple; `None` → host.
    pub target: Option<String>,
    /// `--host <package>`: the host package the build is for; see `host_modules`.
    pub host: Option<HostBuild>,
    /// The CPU to build for (`--cpu`, `$VELT_CPU` or the package's `build.cpu`, resolved by
    /// `commands::cpu::resolve_cpu`); `None` → the target's default
    /// (`velt_codegen_cl::DEFAULT_X86_CPU` on x86_64).
    pub cpu: Option<String>,
    /// What to produce.
    pub emit: Emit,
    /// Code generator; `None` → [`Backend::resolve`] (LLVM for release builds when clang exists).
    pub backend: Option<Backend>,
    /// `--report numbers`: add the number representation report to [`super::Session::reports`].
    pub report_numbers: bool,
    /// Describe local variables for debuggers in a debug build (`velt build`, `velt dev --exe`).
    /// Off for builds that only run (`velt run`, `velt test`): the backend also stores each
    /// assignment of a described variable to a stack slot (+7–15% instructions on benchmarks).
    pub debug_vars: bool,
    /// `--no-generators`: an instance of a generated function that has no code is an error
    /// naming the generator that would write it, and no generator runs (`crate::generate`).
    pub no_generators: bool,
    /// Compare the variants of every `#name` import the program resolves
    /// ([`velt_loader::LoadOptions::check_variants`]): `velt check` and `velt test`, not the builds
    /// that only run.
    pub check_variants: bool,
    /// Check std's own `declare c("c")` functions against the C library's headers too
    /// (`velt native headers --std`; `native::headers::STD_HEADERS`): with zig, so not in
    /// builds, where std's declarations are taken as checked.
    pub std_c_headers: bool,
}

/// The host package of a `--host` build (`vpm::host`): what its `package.velt`'s `host` says, and
/// where its files are. The compiler knows no platform: this is everything about one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HostBuild {
    /// The program's name (a file's stem, a package's name without its scope): the default
    /// directory's, and `{{name}}` in scaffolded files.
    pub program: String,
    /// The package's name, as `--host` gave it.
    pub package: String,
    /// The package's root (its `host` paths are relative to it).
    pub root: PathBuf,
    /// Its `host` object.
    pub config: vpm::host::HostConfig,
}

impl BuildOptions {
    /// The target triple to compile for (host by default).
    pub fn target(&self) -> String {
        self.target
            .clone()
            .unwrap_or_else(velt_codegen_cl::host_triple)
    }

    /// Whether the output carries debug info (backends emit it exactly when the VIR has
    /// source locations).
    pub fn wants_debug_info(&self) -> bool {
        !self.release || self.debug_info
    }

    /// Whether the program keeps line information for debuggers: debug info was asked for, and
    /// it reaches the executable. Cranelift's DWARF in Windows objects reaches it only through
    /// the bundled `lld-link`; MSVC's `link.exe` keeps the sections but cuts their names to 8
    /// bytes, so with it the build emits none (an object, `--emit obj`, keeps it: whoever links
    /// it chooses the linker).
    pub fn has_line_info(&self) -> bool {
        let target = self.target();
        let bundled = || {
            matches!(
                velt_link::linker_report(&target),
                Ok(velt_link::LinkerReport::Bundled { .. })
            )
        };
        self.line_info(bundled)
    }

    /// [`has_line_info`](Self::has_line_info), with `bundled` answering whether the bundled lld
    /// links for the target.
    pub fn line_info(&self, bundled: impl FnOnce() -> bool) -> bool {
        let linked_cranelift_coff = self.target().contains("windows")
            && self.backend != Some(Backend::Llvm)
            && self.emit == Emit::Exe;
        self.wants_debug_info() && (!linked_cranelift_coff || bundled())
    }
}
