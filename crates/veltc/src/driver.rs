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
use crate::loader::{self, LoadOptions};

mod cpu_check;
mod output;
pub(crate) mod outside;
mod pipeline;
mod recover;
pub(crate) mod stamp;
mod std_cache;

pub use output::OutputPaths;
use pipeline::on_pipeline_thread;

/// What to build and how.
#[derive(Clone, Debug, Default)]
pub struct BuildOptions {
    /// Root module file (with `root_source`: the virtual path relative imports resolve from).
    pub input: PathBuf,
    /// Root module source instead of reading `input` (generated `velt test` harnesses).
    pub root_source: Option<String>,
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
    /// All loaded source files.
    pub sm: SourceMap,
    /// Diagnostics from every stage so far.
    pub diagnostics: Diagnostics,
    /// Per-stage wall-clock times.
    pub timings: Vec<(&'static str, Duration)>,
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

    /// Per-stage timing report for `--verbose` (with each stage's breakdown for `--timings`).
    pub fn render_timings(&self) -> String {
        let total: Duration = self.timings.iter().map(|(_, d)| *d).sum();
        let mut s = String::new();
        for (name, d) in self
            .timings
            .iter()
            .map(|(n, d)| (*n, *d))
            .chain([("total", total)])
        {
            s.push_str(&format!(
                "velt: {name:<8} {:>9.3} ms",
                d.as_secs_f64() * 1e3
            ));
            match self.cached {
                Some((found, of, what)) if name == "codegen" => {
                    s.push_str(&format!(
                        "  ({found} of {of} {what} from the build cache)\n"
                    ));
                }
                _ if name == "stamp" && !self.timings.iter().any(|(n, _)| *n == "parse") => {
                    s.push_str("  (unchanged: nothing to compile)\n");
                }
                _ => s.push('\n'),
            }
            let steps = self.details.iter().filter(|(stage, ..)| *stage == name);
            for (_, step, d) in steps.filter(|_| self.show_details) {
                s.push_str(&format!(
                    "velt:   {step:<14} {:>9.3} ms\n",
                    d.as_secs_f64() * 1e3
                ));
            }
        }
        s
    }

    fn record(&mut self, stage: &'static str, start: Instant) {
        self.timings.push((stage, start.elapsed()));
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
    let loaded = load_stage(sess, opts, scope)?;
    sema_stage(sess, opts, scope, &loaded)
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
    let load = LoadOptions {
        std_root: loader::std_root(),
        packages: opts
            .packages
            .as_ref()
            .map(|g| g as &dyn loader::PackageResolver),
        root_source: opts.root_source.clone(),
        overlay: None,
        std_root_as_std: false,
    };
    let loaded = loader::load_with_roots(
        &mut sess.sm,
        &opts.input,
        &scope.extra_roots,
        load,
        &mut sess.diagnostics,
    )
    .map_err(BuildError::Failed)?;
    sess.record("parse", t);
    Ok(loaded)
}

/// Type-check the loaded program (and the native declarations against their libraries).
fn sema_stage(
    sess: &mut Session,
    opts: &BuildOptions,
    scope: &CheckScope,
    loaded: &loader::Loaded,
) -> Result<hir::Program, BuildError> {
    let t = Instant::now();
    let sema_opts = velt_sema::CheckOptions {
        require_main: scope.require_main,
        eager_std: scope.eager_std,
    };
    let hir = crate::generate::check(sess, &loaded.modules, loaded.root, sema_opts);
    if let Some(hir) = &hir {
        let std_root = loader::std_root();
        let std_files = loaded
            .modules
            .iter()
            .filter(|m| m.is_std)
            .map(|m| m.file)
            .collect();
        crate::native::check_declares(
            hir,
            &sess.sm,
            std_root.as_deref(),
            opts.packages.as_ref(),
            &std_files,
            &opts.target(),
            &mut sess.diagnostics,
        );
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
    let hir = check_program(sess, opts, &scope)?;

    let t = Instant::now();
    let std_root = loader::std_root();
    let native_inits = crate::native::inits(opts.packages.as_ref());
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
    };
    let mut program = match velt_vir::lower_checked(&hir, &lower_opts) {
        Ok(p) => p,
        Err(diags) => {
            sess.diagnostics.extend(diags);
            sess.stop_if_errors()?;
            return Err(BuildError::Ice("lowering reported no errors".into()));
        }
    };
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

/// [`check_with`] for a lint of the checked modules (`velt check --ts-compat`): the loaded
/// modules once the type checker ran, with its outcome (whose errors are in the session).
/// `Err` when loading or parsing failed, so nothing was type-checked.
pub fn check_for_lint(
    sess: &mut Session,
    opts: &BuildOptions,
    scope: &CheckScope,
) -> Result<(loader::Loaded, Result<(), BuildError>), BuildError> {
    on_pipeline_thread(|| {
        let loaded = load_stage(sess, opts, scope)?;
        let checked = sema_stage(sess, opts, scope, &loaded).map(drop);
        Ok((loaded, checked))
    })
}

fn build_on_current_thread(
    sess: &mut Session,
    opts: &BuildOptions,
) -> Result<Artifact, BuildError> {
    let target = opts.target();
    // An executable whose inputs are those of its last build is up to date (`stamp`).
    let stamped = (opts.emit == Emit::Exe && !opts.report_numbers && stamp::enabled()).then(|| {
        let t = Instant::now();
        let backend = Backend::resolve(opts.backend, opts.release).0;
        let key = stamp::options_key(opts, backend);
        let paths = OutputPaths::new(&opts.input, opts.output.as_deref(), opts.emit, &target);
        let stamp_path = stamp::path(&paths.executable);
        let objects = stamp::read(&stamp_path, &key)
            .map(|(n, std)| {
                (0..n)
                    .map(|i| paths.unit_object(i))
                    .chain(std)
                    .collect::<Vec<_>>()
            })
            .filter(|objects| objects.iter().all(|o| o.is_file()));
        if objects.is_none() {
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
    // What module resolution looked at, for the stamp.
    let (program, seen) = match &stamped {
        Some(_) => vpm::probe::record(|| {
            stamp::note_project(opts);
            compile_to_vir(sess, opts)
        }),
        None => (compile_to_vir(sess, opts), vpm::probe::Seen::default()),
    };
    let mut program = program?;
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
        .emit_objects(&program, &cg, units, &mut steps)
        .map_err(|e| BuildError::Failed(format!("code generation failed: {e}")))?;
    sess.record("codegen", t);
    sess.cached = cached.map(|c| match backend {
        Backend::Cranelift => (c.hits, c.hits + c.misses, "functions"),
        Backend::Llvm => (c.hits, c.hits + c.misses, "codegen units"),
    });
    sess.record_details("codegen", &steps);

    let t = Instant::now();
    let mut object_paths = Vec::with_capacity(objects.len());
    for (i, obj) in objects.iter().enumerate() {
        let path = paths.unit_object(i);
        write_file(&path, obj)?;
        object_paths.push(path);
    }
    sess.record("write", t);
    if opts.emit == Emit::Obj {
        // `-o lib.o` must not delete a `lib.cgu1.o` of the user's.
        return Ok(Artifact::Object(paths.object));
    }
    paths.remove_unit_objects_from(objects.len());

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
        };
        // Best effort: without a stamp the next build compiles again.
        let _ = stamp::write(&stamp_path, &stamp::render(&key, &sess.sm, &built));
    }
    Ok(Artifact::Executable(paths.executable))
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
    let linked = crate::link::link_executable(
        target,
        objects,
        &paths.executable,
        opts.release,
        // Release settings strip debug info (and PDB generation on Windows).
        opts.release && !opts.debug_info,
        &crate::native::links(opts.packages.as_ref(), opts.release),
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
mod tests {
    use super::*;

    #[test]
    fn missing_input_is_an_error_not_a_panic() {
        let mut sess = Session::new();
        let opts = BuildOptions {
            input: PathBuf::from("no/such/file.vlt"),
            ..Default::default()
        };
        match build(&mut sess, &opts) {
            Err(BuildError::Failed(m)) => assert!(m.contains("cannot read"), "{m}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn stops_after_first_failing_stage() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("bad.vlt");
        std::fs::write(&file, "function main( {").unwrap();
        let mut sess = Session::new();
        let r = compile_to_vir(
            &mut sess,
            &BuildOptions {
                input: file,
                ..Default::default()
            },
        );
        assert!(matches!(r, Err(BuildError::Diagnostics)));
        assert!(sess.has_errors());
        assert_eq!(
            sess.timings.len(),
            1,
            "only the parse stage should have run"
        );
        assert!(sess.render_diagnostics().contains("bad.vlt:"));
    }

    #[test]
    fn root_source_replaces_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let opts = BuildOptions {
            input: dir.path().join("virtual.vlt"),
            root_source: Some("function main( {".into()),
            ..Default::default()
        };
        let mut sess = Session::new();
        assert!(matches!(
            compile_to_vir(&mut sess, &opts),
            Err(BuildError::Diagnostics)
        ));
        assert!(sess.render_diagnostics().contains("virtual.vlt:"));
    }
}
