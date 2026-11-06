//! The compilation pipeline: load → parse → sema → lower → verify → codegen → link.
//! Library-style so `velt build`, `velt run`, `velt test` and tests share it. The pipeline runs on a
//! dedicated thread with a large stack: recursive passes over deeply nested ASTs would overflow the
//! 1 MB Windows main thread.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use velt_common::{Diagnostics, SourceMap};
use velt_sema::hir;
use velt_vir::vir;

use crate::backend::Backend;
use crate::cli::Emit;
use velt_loader as loader;

mod cpu_check;
mod host_modules;
pub(crate) mod memory;
mod output;
pub(crate) mod outside;
mod pipeline;
mod recover;
mod std_load;
use std_load::load_options;
pub(crate) use std_load::StdLoad;
pub(crate) mod stamp;
mod std_cache;
mod timings;

pub use output::{default_dir, OutputPaths};
use pipeline::on_pipeline_thread;

/// What to build and how.
#[derive(Clone, Debug, Default)]
pub struct BuildOptions {
    /// Root module file (with `root_source`: the virtual path relative imports resolve from).
    pub input: PathBuf,
    /// Root module source instead of reading `input` (generated `velt test` harnesses).
    pub root_source: Option<String>,
    /// The root is `velt test`'s generated harness, which imports the test runner's hooks from
    /// velt/test's internal files ([`loader::LoadOptions::test_harness`]).
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
    /// `--report numbers`: add the number representation report to [`Session::reports`].
    pub report_numbers: bool,
    /// Describe local variables for debuggers in a debug build (`velt build`, `velt dev --exe`).
    /// Off for builds that only run (`velt run`, `velt test`): the backend also stores each
    /// assignment of a described variable to a stack slot (+7–15% instructions on benchmarks).
    pub debug_vars: bool,
    /// `--no-generators`: an instance of a generated function that has no code is an error
    /// naming the generator that would write it, and no generator runs (`crate::generate`).
    pub no_generators: bool,
    /// Compare the variants of every `#name` import the program resolves
    /// ([`loader::LoadOptions::check_variants`]): `velt check` and `velt test`, not the builds
    /// that only run.
    pub check_variants: bool,
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

/// What [`check_with`] checks besides [`BuildOptions::input`].
#[derive(Clone, Debug, Default)]
pub struct CheckScope {
    /// The root must define `main` (a package's entry); without it the root may be a library
    /// module (a `main` that is there is validated either way).
    pub require_main: bool,
    /// Further modules checked as library modules in the same run (`velt check` in a package:
    /// every module under `src/` and `tests/`); modules they share are loaded and checked once.
    pub extra_roots: Vec<PathBuf>,
    /// Check every definition of std the loaded modules hold, reached or not (`velt check
    /// --std`).
    pub eager_std: bool,
}

/// What a successful build produced.
#[derive(Debug)]
pub enum Artifact {
    /// `--emit vir`: the VIR text (`Display`).
    Vir(String),
    /// `--emit llvm`: the LLVM IR text.
    Llvm(String),
    /// `--emit obj`: the object file.
    Object(PathBuf),
    /// The linked executable.
    Executable(PathBuf),
}

/// Why a build stopped.
#[derive(Debug)]
pub enum BuildError {
    /// Stage errors; they are in [`Session::diagnostics`].
    Diagnostics,
    /// A non-source error (IO, codegen, linker) — printed as `error: <msg>`.
    Failed(String),
    /// Internal compiler error (e.g. VIR verification failed).
    Ice(String),
}

/// State of one compiler invocation: sources, accumulated diagnostics (errors and warnings), timings.
#[derive(Default)]
pub struct Session {
    /// The files of the std modules the program loaded, those loaded during the check included.
    pub std_files: Vec<velt_common::FileId>,
    /// All loaded source files.
    pub sm: SourceMap,
    /// Diagnostics from every stage so far.
    pub diagnostics: Diagnostics,
    /// Per-stage wall-clock times.
    pub timings: Vec<(&'static str, Duration)>,
    /// The compiler's memory at the end of each stage, printed with `--timings`.
    pub memory: Vec<(&'static str, memory::Reading)>,
    /// When each stage ended and when its data was freed.
    pub lifetimes: memory::Lifetimes,
    /// Breakdown of some stages: `(stage, step, time)` (optimizer passes, codegen steps).
    pub details: Vec<(&'static str, &'static str, Duration)>,
    /// Print [`Session::details`] under their stage (`--timings`).
    pub show_details: bool,
    /// Reports the build was asked for (`--report`), printed after the diagnostics.
    pub reports: Vec<String>,
    /// What codegen took from the build cache: `(found, of, what)` ("functions", "codegen
    /// units"); `None` without a cache.
    pub cached: Option<(usize, usize, &'static str)>,
    /// Objects of std functions the program links instead of compiling (`std_cache`).
    pub std_objects: Vec<PathBuf>,
    /// What the source generators the build ran read, as build-stamp `file` entries
    /// (`crate::generate`): a changed generator rebuilds the program. The C headers of its
    /// constants and C functions too (`native::read_c_constants`, `native::c_stamp_inputs`).
    pub generator_inputs: Vec<String>,
    /// This builds a source generator host, whose generators must not reach outside the
    /// program (no I/O, clock, randomness or native code: [`check_generator_host`]).
    pub generator_build: bool,
    /// Run no source generator: what would need one is an error ([`BuildOptions::no_generators`]).
    pub no_generators: bool,
    /// The errors source generators gave (also in `diagnostics`): what they reported, and their
    /// failures (`crate::generate::report`). The editor shows these after its own check.
    pub generator_reports: Diagnostics,
    /// The system libraries of the C libraries the program's `declare c` functions name
    /// (`crate::native::c_links`: `sqlite3`, `framework:Accelerate`), set by the check.
    pub c_libs: Vec<String>,
    /// The packages with native code the program allows, and their unsafe code (set by the
    /// check; `velt check` lists them).
    pub native: Vec<crate::native::NativePackage>,
    /// The digests of the C declarations of packages built here that passed the header check,
    /// per package root and library (set by the check; `velt native headers` records them).
    pub c_checked: crate::native::CChecked,
    /// The `declare c const` values read from headers here, for `c-headers.json`; set by the check.
    pub c_constants: crate::native::CConstants,
}

impl Session {
    /// An empty session.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether any error diagnostic was reported.
    pub fn has_errors(&self) -> bool {
        self.diagnostics.iter().any(|d| d.is_error())
    }

    /// All diagnostics rendered, separated by one blank line (empty if none).
    pub fn render_diagnostics(&self) -> String {
        let parts: Vec<String> = self
            .diagnostics
            .iter()
            .map(|d| d.render(&self.sm))
            .collect();
        parts.join("\n\n")
    }

    fn record(&mut self, stage: &'static str, start: Instant) {
        self.record_time(stage, start.elapsed());
    }

    /// Record that `stage` took `d`, and the memory at its end for `--timings`.
    pub(crate) fn record_time(&mut self, stage: &'static str, d: Duration) {
        self.timings.push((stage, d));
        self.lifetimes.push(stage);
        if self.show_details {
            self.memory.extend(memory::read().map(|r| (stage, r)));
        }
    }

    /// Record the breakdown `steps` of `stage` (printed under it with `--timings`).
    pub fn record_details(&mut self, stage: &'static str, steps: &[(&'static str, Duration)]) {
        let steps = steps.iter().map(|&(step, d)| (stage, step, d));
        self.details.extend(steps);
    }

    fn stop_if_errors(&self) -> Result<(), BuildError> {
        if self.has_errors() {
            Err(BuildError::Diagnostics)
        } else {
            Ok(())
        }
    }
}

/// Front end only: load + parse + sema → typed HIR (what `velt check` runs). Stops after the
/// first stage with errors. Everything but `velt check` requires `main` and has no extra roots.
fn check_program(
    sess: &mut Session,
    opts: &BuildOptions,
    scope: &CheckScope,
) -> Result<hir::Program, BuildError> {
    let mut loaded = load_stage(sess, opts, scope)?;
    sema_stage(sess, opts, scope, &mut loaded)
}

/// Load and parse the program; stops if that reported errors.
fn load_stage(
    sess: &mut Session,
    opts: &BuildOptions,
    scope: &CheckScope,
) -> Result<loader::Loaded, BuildError> {
    let loaded = load_modules(sess, opts, scope)?;
    sess.stop_if_errors()?;
    Ok(loaded)
}

/// Load and parse the program, recovering from syntax errors (they are in the session).
fn load_modules(
    sess: &mut Session,
    opts: &BuildOptions,
    scope: &CheckScope,
) -> Result<loader::Loaded, BuildError> {
    let t = Instant::now();
    let load = load_options(sess.generator_build, opts, scope);
    let loaded = loader::load_program(&mut sess.sm, &opts.input, load, &mut sess.diagnostics)
        .map_err(BuildError::Failed)?;
    sess.record("parse", t);
    Ok(loaded)
}

/// Type-check the loaded program (and the native declarations against their libraries).
fn sema_stage(
    sess: &mut Session,
    opts: &BuildOptions,
    scope: &CheckScope,
    loaded: &mut loader::Loaded,
) -> Result<hir::Program, BuildError> {
    let t = Instant::now();
    let sema_opts = velt_sema::CheckOptions {
        require_main: scope.require_main,
        eager_std: scope.eager_std,
    };
    let packages = opts.packages.as_ref();
    sess.no_generators |= opts.no_generators;
    let demands = !scope.eager_std && loader::demand_std_on();
    let generator_build = sess.generator_build;
    let mut more = demands.then(|| {
        StdLoad::new(
            (&mut loaded.state, &loaded.modules),
            generator_build,
            opts,
            scope,
        )
    });
    let mut hir = crate::generate::check(
        sess,
        &loaded.modules,
        loaded.root,
        sema_opts,
        packages,
        more.as_mut(),
    );
    sess.std_files = StdLoad::std_files(more, &loaded.modules);
    sess.native = crate::native::check_trust(
        &loaded.modules,
        &sess.sm,
        opts.packages.as_ref(),
        &opts.target(),
        &mut sess.diagnostics,
    );
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
            std_root.as_deref(),
            opts.packages.as_ref(),
            &std_files,
            &opts.target(),
            &mut sess.diagnostics,
        );
        crate::native::note_checked(&mut sess.native, opts.packages.as_ref(), &sess.c_checked);
        crate::native::c_stamp_inputs(&sess.c_checked, &mut sess.generator_inputs);
        sess.c_libs = crate::native::c_links(hir, &sess.sm, opts.packages.as_ref(), &opts.target());
    }
    let _ = t;
    sess.stop_if_errors()?;
    hir.ok_or_else(|| BuildError::Ice("sema returned no program but reported no errors".into()))
}

/// Front half of the pipeline: source file → verified VIR. Stops after the first stage with errors.
pub fn compile_to_vir(sess: &mut Session, opts: &BuildOptions) -> Result<vir::Program, BuildError> {
    let scope = CheckScope {
        require_main: true,
        extra_roots: vec![],
        eager_std: false,
    };
    let hir = memory::Noted::new(
        check_program(sess, opts, &scope)?,
        "hir freed",
        &sess.lifetimes,
    );

    let t = Instant::now();
    let std_root = loader::std_root();
    // A generator host reaches no native code (`check_generator_host`): it starts none of the
    // program's native packages and links none of their libraries.
    let native_inits = match sess.generator_build {
        true => vec![],
        false => crate::native::inits(opts.packages.as_ref()),
    };
    let target = opts.target();
    let line_info = opts.has_line_info();
    let lower_opts = velt_vir::LowerOptions {
        source_map: Some(&sess.sm),
        std_root: std_root.as_deref(),
        native_inits: &native_inits,
        // A debug compiler links the debug runtime, which checks that no two tasks use one
        // captured variable's cell (#916); release programs and release compilers skip it.
        cell_checks: !opts.release && cfg!(debug_assertions),
        // Variables for debuggers in debug builds made for debugging that keep their line
        // information (they refer to its files); release builds with `-g` keep line information
        // only, and their optimizations stay as they are.
        debug_info: !opts.release && opts.debug_vars && line_info,
        target: Some(&target),
        // Generic instances shared by shape: debug builds only, for now (shape-sharing.md).
        shapes: !opts.release,
    };
    let mut program = match velt_vir::lower_checked(&hir, &lower_opts) {
        Ok(p) => p,
        Err(diags) => {
            sess.diagnostics.extend(diags);
            sess.stop_if_errors()?;
            return Err(BuildError::Ice("lowering reported no errors".into()));
        }
    };
    host_modules::check_build(sess, opts, &hir, &program)?;
    // Nothing after lowering reads the HIR: freed before the VIR is verified and optimized.
    drop(hir);
    if !line_info && !opts.report_numbers {
        // Panic messages already carry their locations; only debug info needs these.
        program.files.clear();
        program.funcs.iter_mut().for_each(|f| f.locs.clear());
    }
    sess.record("lower", t);

    let t = Instant::now();
    let verified = velt_vir::verify(&program);
    sess.record("verify", t);
    if let Err(errs) = verified {
        return Err(BuildError::Ice(format!(
            "VIR verification failed:\n  {}\n--- VIR ---\n{program}",
            errs.join("\n  ")
        )));
    }

    // The std cache (#1728): std functions out of the program before it is optimized, their
    // objects linked after. The program as lowered is kept to fall back on.
    let cg = velt_codegen_cl::CodegenOptions {
        target: opts.target(),
        optimize: opts.release,
        cpu: opts.cpu.clone(),
    };
    let std_plan = std_cache::wanted(opts).then(|| {
        let t = Instant::now();
        let unsplit = program.clone();
        let plan = std_cache::split(&mut program, opts, &cg);
        sess.record("std cache", t);
        (unsplit, plan)
    });
    optimize_stage(sess, opts, &mut program)?;
    if let Some((unsplit, plan)) = std_plan {
        let t = Instant::now();
        let place =
            OutputPaths::new(&opts.input, opts.output.as_deref(), opts.emit, &target).std_objects();
        match std_cache::link(plan, &program, &cg, &place) {
            Ok(linked) => {
                if std_cache::stats() {
                    eprintln!(
                        "velt: std cache: {} found, {} compiled, {} no longer called",
                        linked.hits, linked.misses, linked.dropped
                    );
                }
                sess.std_objects = linked.objects;
            }
            Err(why) => {
                place.clear();
                if std_cache::stats() {
                    eprintln!("velt: std cache: not used for this build: {why}");
                }
                program = unsplit;
                optimize_stage(sess, opts, &mut program)?;
            }
        }
        sess.record("std cache link", t);
    }
    if opts.report_numbers {
        sess.reports.push(crate::numbers_report::render(&program));
        if !line_info {
            program.files.clear();
            program.funcs.iter_mut().for_each(|f| f.locs.clear());
        }
    }
    Ok(program)
}

/// `velt_opt` over `program` (release builds: `Speed`), then verification.
fn optimize_stage(
    sess: &mut Session,
    opts: &BuildOptions,
    program: &mut vir::Program,
) -> Result<(), BuildError> {
    let t = Instant::now();
    let level = if opts.release {
        velt_opt::OptLevel::Speed
    } else {
        velt_opt::OptLevel::None
    };
    let mut passes = velt_opt::PassTimings::new();
    velt_opt::optimize_timed(program, level, &mut passes);
    let verify_start = Instant::now();
    let verified = velt_vir::verify(program);
    let mut steps = passes.entries().to_vec();
    steps.push(("verify", verify_start.elapsed()));
    sess.record("optimize", t);
    sess.record_details("optimize", &steps);
    verified.map_err(|errs| {
        BuildError::Ice(format!(
            "VIR verification failed after optimization:\n  {}",
            errs.join("\n  ")
        ))
    })
}

/// Full build per `opts.emit`, run on a large-stack thread.
pub fn build(sess: &mut Session, opts: &BuildOptions) -> Result<Artifact, BuildError> {
    on_pipeline_thread(|| build_on_current_thread(sess, opts))
}

/// Source → optimized VIR (no codegen), on a large-stack thread: for the JIT host, and for
/// `velt dev` to check a new version before replacing the running one.
pub fn compile(sess: &mut Session, opts: &BuildOptions) -> Result<vir::Program, BuildError> {
    on_pipeline_thread(|| compile_to_vir(sess, opts))
}

/// Parse + sema only (`velt check`), on a large-stack thread: every diagnostic the front end
/// reports, no lowering, codegen or link. The root need not define `main` (a library module).
pub fn check(sess: &mut Session, opts: &BuildOptions) -> Result<(), BuildError> {
    check_with(sess, opts, &CheckScope::default())
}

/// [`check`] of `opts.input` and `scope`'s extra roots in one front-end run (`velt check` in a
/// package), with `main` required of the root when `scope` says so.
pub fn check_with(
    sess: &mut Session,
    opts: &BuildOptions,
    scope: &CheckScope,
) -> Result<(), BuildError> {
    on_pipeline_thread(|| recover::check_recovering(sess, opts, scope))
}

/// [`check_with`] that also returns the loaded modules, for a lint of them (`velt check
/// --perf`, `--fix`): the same diagnostics as [`check_with`]; `Err` when loading failed.
pub fn check_with_modules(
    sess: &mut Session,
    opts: &BuildOptions,
    scope: &CheckScope,
) -> Result<(loader::Loaded, Result<(), BuildError>), BuildError> {
    on_pipeline_thread(|| recover::check_recovering_loaded(sess, opts, scope))
}

/// [`check_with`] for a lint of the checked modules (`velt check --ts-compat`): the loaded
/// modules once the type checker ran, with its outcome (whose errors are in the session).
/// `Err` when loading or parsing failed, so nothing was type-checked.
pub fn check_for_lint(
    sess: &mut Session,
    opts: &BuildOptions,
    scope: &CheckScope,
) -> Result<(loader::Loaded, Result<(), BuildError>), BuildError> {
    on_pipeline_thread(|| {
        let mut loaded = load_stage(sess, opts, scope)?;
        let checked = sema_stage(sess, opts, scope, &mut loaded).map(drop);
        Ok((loaded, checked))
    })
}

fn build_on_current_thread(
    sess: &mut Session,
    opts: &BuildOptions,
) -> Result<Artifact, BuildError> {
    let target = opts.target();
    // An executable whose inputs are those of its last build is up to date (`stamp`).
    // The hashes of the files the last build's stamp records, for this build to reuse.
    let mut known = Default::default();
    // `--no-generators` checks the program again: an unchanged one may need generators.
    let stamps = !opts.report_numbers && !opts.no_generators && stamp::enabled();
    let stamped = (opts.emit == Emit::Exe && stamps).then(|| {
        let t = Instant::now();
        let backend = Backend::resolve(opts.backend, opts.release).0;
        let key = stamp::options_key(opts, backend);
        let paths = OutputPaths::new(&opts.input, opts.output.as_deref(), opts.emit, &target);
        let stamp_path = stamp::path(&paths.executable);
        let objects = stamp::read(&stamp_path, &key)
            .map(|(n, std, c_libs)| {
                // Not checked again, and the check is what finds them.
                sess.c_libs = c_libs;
                (0..n)
                    .map(|i| paths.unit_object(i))
                    .chain(std)
                    .collect::<Vec<_>>()
            })
            .filter(|objects| objects.iter().all(|o| o.is_file()));
        if objects.is_none() {
            known = stamp::known(&stamp_path);
            // A build that fails half-way must not leave a stamp of the old one.
            let _ = std::fs::remove_file(&stamp_path);
            // Made before the build looks at directories, so their listings include it.
            if let Some(dir) = paths.executable.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
        }
        sess.record("stamp", t);
        (key, paths, stamp_path, objects)
    });
    // A build that writes the executable without a stamp (`VELT_BUILD_STAMP=off`, `--report`)
    // leaves no stamp of an earlier one beside it.
    if stamped.is_none() && opts.emit == Emit::Exe {
        let paths = OutputPaths::new(&opts.input, opts.output.as_deref(), opts.emit, &target);
        let _ = std::fs::remove_file(stamp::path(&paths.executable));
    }
    if let Some((_, paths, _, Some(objects))) = &stamped {
        link_stage(sess, opts, &target, objects, paths)?;
        return Ok(Artifact::Executable(paths.executable.clone()));
    }
    stamp::check_fetched(opts).map_err(BuildError::Failed)?;
    // What module resolution looked at, for the stamp.
    let (program, seen) = match &stamped {
        Some(_) => vpm::probe::record_reusing(known, || {
            stamp::note_project(opts);
            compile_to_vir(sess, opts)
        }),
        None => (compile_to_vir(sess, opts), vpm::probe::Seen::default()),
    };
    let mut program = memory::Noted::new(program?, "vir freed", &sess.lifetimes);
    check_generator_host(sess, &program)?;
    // For the stamp: what the program reaches outside itself (`velt test` keeps a result only
    // for a program that reaches nothing).
    let reaches = outside::reaches(&program);
    if opts.emit == Emit::Vir {
        return Ok(Artifact::Vir(program.to_string()));
    }
    let cg = velt_codegen_cl::CodegenOptions {
        target: target.clone(),
        optimize: opts.release,
        cpu: opts.cpu.clone(),
    };
    // `--emit llvm` prints what the LLVM backend compiles.
    let backend = match opts.emit {
        Emit::Llvm => Backend::Llvm,
        _ => Backend::resolve(opts.backend, opts.release).0,
    };
    let required = backend
        .required_features(&target, opts.cpu.as_deref())
        .map_err(BuildError::Failed)?;
    cpu_check::insert(&mut program, required);
    if opts.emit == Emit::Llvm {
        return velt_codegen_llvm::emit_ir_for(&program, &cg)
            .map(Artifact::Llvm)
            .map_err(|e| BuildError::Failed(format!("code generation failed: {e}")));
    }
    let paths = OutputPaths::new(&opts.input, opts.output.as_deref(), opts.emit, &target);

    let t = Instant::now();
    if backend == Backend::Cranelift && velt_codegen_llvm::is_wasm(&target) {
        return Err(BuildError::Failed(format!(
            "`{target}` needs the LLVM backend (`--backend llvm`)"
        )));
    }
    let mut steps = vec![];
    // `--emit obj` promises one object file.
    let units = if opts.emit == Emit::Obj {
        Some(1)
    } else {
        codegen_units()
    };
    let (objects, cached) = backend
        .emit_objects(&mut program, &cg, units, &mut steps)
        .map_err(|e| BuildError::Failed(format!("code generation failed: {e}")))?;
    // The objects are all the rest of the build needs: the VIR is freed before they are written
    // and linked.
    drop(program);
    sess.record("codegen", t);
    sess.cached = cached.map(|c| match backend {
        Backend::Cranelift => (c.hits, c.hits + c.misses, "functions"),
        Backend::Llvm => (c.hits, c.hits + c.misses, "codegen units"),
    });
    sess.record_details("codegen", &steps);

    let t = Instant::now();
    let mut object_paths = Vec::with_capacity(objects.len());
    // Each object is freed once written: the linker reads them from their files.
    for (i, obj) in objects.into_iter().enumerate() {
        let path = paths.unit_object(i);
        write_file(&path, &obj)?;
        object_paths.push(path);
    }
    sess.record("write", t);
    if opts.emit == Emit::Obj {
        // `-o lib.o` must not delete a `lib.cgu1.o` of the user's.
        return Ok(Artifact::Object(paths.object));
    }
    paths.remove_unit_objects_from(object_paths.len());

    // The std cache's objects are in place (`std_cache::link`); a build without them leaves
    // none of an earlier one's.
    if sess.std_objects.is_empty() {
        paths.std_objects().clear();
    }
    let mut linked = object_paths.clone();
    linked.extend(sess.std_objects.iter().cloned());
    link_stage(sess, opts, &target, &linked, &paths)?;
    crate::build_cache::collect_garbage_if_due();
    // Warnings are shown by every build, so a build with diagnostics gets no stamp.
    if let Some((key, _, stamp_path, _)) = stamped.filter(|_| sess.diagnostics.is_empty()) {
        let generated = opts.root_source.as_ref().map(|_| opts.input.as_path());
        // The outputs too: another build or tool that rewrites them (`--emit obj` onto a unit
        // object, an older velt) makes this stamp stale.
        let mut outputs = object_paths.clone();
        outputs.push(paths.executable.clone());
        let built = stamp::Built {
            seen: &seen,
            virtual_root: generated,
            objects: object_paths.len(),
            link_inputs: &outputs,
            std_objects: &sess.std_objects,
            outside: &reaches,
            generator_inputs: &sess.generator_inputs,
            c_libs: &sess.c_libs,
        };
        // Best effort: without a stamp the next build compiles again.
        let _ = stamp::write(&stamp_path, &stamp::render(&key, &sess.sm, &built));
    }
    Ok(Artifact::Executable(paths.executable))
}

/// In a source generator host ([`Session::generator_build`]): an error when the generators reach
/// outside the program. A generator is a pure function of its request, which makes its output
/// cacheable and running it at compile time safe (docs/internals/design/source-generators.md §5).
pub fn check_generator_host(sess: &mut Session, program: &vir::Program) -> Result<(), BuildError> {
    if !sess.generator_build {
        return Ok(());
    }
    let reaches = outside::generator_reaches(program);
    if reaches == "-" {
        return Ok(());
    }
    Err(BuildError::Failed(format!(
        "a source generator cannot reach {reaches}: it runs at compile time and gets everything \
         from its request"
    )))
}

/// Link `objects` into `paths.executable` (nothing to do when its link stamp matches).
fn link_stage(
    sess: &mut Session,
    opts: &BuildOptions,
    target: &str,
    objects: &[PathBuf],
    paths: &OutputPaths,
) -> Result<(), BuildError> {
    let t = Instant::now();
    let (packages, crates) = (opts.packages.as_ref(), !sess.generator_build);
    let native = crate::native::program_links(packages, &sess.c_libs, crates, opts.release, target);
    let linked = crate::link::link_executable(
        target,
        objects,
        &paths.executable,
        opts.release,
        // Release settings strip debug info (and PDB generation on Windows).
        opts.release && !opts.debug_info,
        &native.map_err(BuildError::Failed)?,
        &crate::native::c_link_args(&sess.c_libs, target),
    )
    .map_err(BuildError::Failed)?;
    sess.record("link", t);
    if linked == crate::link::Linked::UpToDate {
        sess.record_details("link", &[("up to date", t.elapsed())]);
    }
    Ok(())
}

/// `$VELT_CODEGEN_UNITS`: how many codegen units the LLVM backend splits a program into, at most
/// the core count (unset or not a positive number: `None`, the backend picks the count from the
/// program's size).
fn codegen_units() -> Option<usize> {
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    std::env::var("VELT_CODEGEN_UNITS")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .filter(|&n: &usize| n > 0)
        .map(|n: usize| n.min(cores))
}

fn write_file(path: &Path, bytes: &[u8]) -> Result<(), BuildError> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)
            .map_err(|e| BuildError::Failed(format!("cannot create `{}`: {e}", dir.display())))?;
    }
    std::fs::write(path, bytes)
        .map_err(|e| BuildError::Failed(format!("cannot write `{}`: {e}", path.display())))
}

#[cfg(test)]
mod tests;
