//! The compilation pipeline: load → parse → sema → lower → verify → codegen → link.
//! Library-style so `velt build`, `velt run`, `velt test` and tests share it. The pipeline runs on a
//! dedicated thread with a large stack: recursive passes over deeply nested ASTs would overflow the
//! 1 MB Windows main thread.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use velt_common::{Diagnostics, SourceMap};
use velt_sema::hir;
use velt_vir::vir;

use crate::backend::Backend;
use crate::cli::Emit;
use velt_loader as loader;

mod cpu_check;
mod executable;
mod heap_hooks;
mod host_modules;
pub(crate) mod memory;
mod options;
mod output;
pub(crate) mod outside;
mod pipeline;
mod recover;
mod std_load;
mod std_summary;
use std_load::load_options;
pub(crate) use std_load::StdLoad;
pub(crate) mod units_disk;
mod units_twice;
mod warm;
pub use warm::{compile_for_jit_warm, copy_files};
pub(crate) mod stamp;
mod std_cache;
pub(crate) use std_cache::prebuilt_warm;
mod timings;

use executable::build_on_current_thread;
pub use options::{BuildOptions, HostBuild};
use output::{codegen_units, link_stage, write_file};
pub use output::{default_dir, OutputPaths};
use pipeline::on_pipeline_thread;

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
    /// Check by units (`velt_sema::CheckOptions::units`; `VELT_UNITS` for `velt check`).
    pub units: bool,
    /// A `hookCall` of a prebuilt hook may call its module's object, where the build cache holds
    /// it, instead of loading the module (`std_cache::prebuilt`): builds that link, and `velt
    /// check`.
    pub prebuilt: bool,
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
    /// `velt dev`: the counted types of the previous version, which lowering starts from, and
    /// after lowering the new version's (`velt_vir::lower_seeded`). `None`: lowering as always.
    pub counted_seed: Option<velt_vir::CountedSeed>,
    /// `velt dev`: `Some` asks for the checked program's interface summary, which replaces it
    /// (`velt_sema::interface`). `None`: none is made.
    pub interfaces: Option<velt_sema::interface::Summary>,
    /// `velt dev`: keep the check of the program (`velt_sema::KeptCheck`, in [`Session::kept`]),
    /// when it can be kept (dev-warm-front-end.md step 3, "C2").
    pub keep_check: bool,
    /// The kept check of the last build, owning the program's parsed modules.
    pub kept: Option<velt_sema::KeptCheck>,
    /// Why a warm reload (`compile_for_jit_warm`) needs the cold front end.
    pub warm_refused: Option<String>,
    /// EXPERIMENT (`driver::units_disk`): std's sources' key, whether std's modules were loaded
    /// as stubs, and that they must not be (sema asked for std).
    pub units_std_key: Option<u64>,
    pub units_stubbed: bool,
    pub units_no_stubs: bool,
    /// `velt dev`: keep the lowering of a JIT build ([`Session::kept_lowering`]).
    pub keep_lowering: bool,
    /// The kept lowering (velt_vir `KeptLowering`): a warm reload lowers its edited files into
    /// it ([`Session::splice_files`]).
    pub kept_lowering: Option<velt_vir::KeptLowering>,
    /// A warm reload's edited files, whose functions are lowered into the kept lowering.
    pub splice_files: Option<Vec<velt_common::FileId>>,
    /// Why the kept lowering could not take a warm reload.
    pub splice_refused: Option<String>,
    /// The last `velt dev` version's optimized program, which a splice changes.
    pub dev_program: Option<vir::Program>,
    /// What the debug pipeline kept of its run over [`Session::dev_program`].
    pub debug_kept: Option<velt_opt::DebugKept>,
    /// The functions a splice changed in the program the build gives (a JIT reload compares
    /// only those with the running version).
    pub dev_changed: Option<Vec<usize>>,
    /// The check's kept check and what it borrows, until the driver hands them over.
    pub(crate) pending_kept: Option<KeptHandover>,
    /// `-v`: say at once that the build waits for another of its output (`std_cache::Place::lock`).
    pub verbose: bool,
    /// Reports the build was asked for (`--report`), printed after the diagnostics.
    pub reports: Vec<String>,
    /// What codegen took from the build cache: `(found, of, what)` ("functions", "codegen
    /// units"); `None` without a cache.
    pub cached: Option<(usize, usize, &'static str)>,
    /// Objects of std functions the program links instead of compiling (`std_cache`).
    pub std_objects: Vec<PathBuf>,
    /// The prebuilt hook modules' objects the check found and called (`std_cache::prebuilt`).
    pub(crate) prebuilt: Vec<(&'static str, std_cache::Found)>,
    /// This build's turn at its output (`std_cache::Place::lock`), until the build returns.
    pub turn: Option<std::fs::File>,
    /// What the program reaches outside itself (`outside::reaches`), when it was worked out
    /// before those functions became declarations.
    pub reaches: Option<String>,
    /// What the source generators the build ran read, as build-stamp `file` entries
    /// (`crate::generate`): a changed generator rebuilds the program. The C headers of its
    /// constants and C functions too (`native::read_c_constants`, `native::c_stamp_inputs`).
    pub generator_inputs: Vec<String>,
    /// This builds a source generator host, whose generators must not reach outside the
    /// program (no I/O, clock, randomness or native code: [`check_generator_host`]).
    pub generator_build: bool,
    /// EXPERIMENT (#2943): this session only checks (`velt check`, [`recover`]): sema's
    /// `CheckOptions::check_only`.
    pub check_only: bool,
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
    let hir = sema_stage(sess, opts, scope, &mut loaded);
    // The kept check borrows the parsed modules: it takes them over, or nothing is kept.
    if let Some(h) = sess.pending_kept.take() {
        let input = h
            .input
            .unwrap_or_else(|| std::mem::take(&mut loaded.modules));
        // SAFETY: `input` is the vector the check was given: `loaded.modules`, alive and unchanged
        // since `sema_stage`, or `check_offered`'s copy, moved here unchanged; `h.sources` and
        // `h.loads` are the generated modules and the `StdLoads` that check was given, moved here
        // unchanged (`generate::check`, `check_offered`).
        let adopted = unsafe { h.pending.adopt(input, h.sources, h.loads) };
        sess.kept = match (&hir, adopted) {
            (Ok(_), Ok(kept)) => Some(kept),
            _ => None,
        };
    }
    hir
}

/// A kept check (`velt_sema::PendingKept`) and the modules it borrows besides the parsed ones,
/// until [`check_program`] hands them over.
pub(crate) struct KeptHandover {
    pub pending: velt_sema::PendingKept,
    /// The modules the check was given when they are a copy of the parsed ones (generated code
    /// checked as program modules); `None`: the parsed modules themselves.
    pub input: Option<Vec<velt_sema::SourceModule>>,
    /// The generated modules given with them.
    pub sources: Vec<velt_sema::SourceModule>,
    pub loads: Option<velt_sema::StdLoads>,
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

/// Load and parse the program, recovering from syntax errors (they are in the session). The
/// fetched packages are checked first ([`stamp::check_fetched`]): every build that compiles and
/// every `velt check` comes here.
fn load_modules(
    sess: &mut Session,
    opts: &BuildOptions,
    scope: &CheckScope,
) -> Result<loader::Loaded, BuildError> {
    stamp::check_fetched(opts).map_err(BuildError::Failed)?;
    let t = Instant::now();
    let prepared = units_disk::prepare(scope, opts);
    if prepared.is_some() {
        sess.record("units: std key and stubs", t);
    }
    let stubs = (prepared.as_ref())
        .filter(|_| !sess.units_no_stubs)
        .and_then(|p| p.stubs.as_ref());
    let decode = |path: &str, file: velt_common::FileId| {
        (stubs.and_then(|s| s.get(path))).and_then(|b| velt_syntax::decode_module(b, file))
    };
    let mut load = load_options(sess.generator_build, opts, scope);
    let std_modules = (prepared.as_ref()).and_then(|p| p.modules.as_deref());
    if stubs.is_some() {
        load.std_stubs = Some(&decode);
        // std's variants were compared when its unit was made; stubs hold no exports to compare.
        load.check_variants = false;
        load.std_modules = std_modules;
    }
    sess.units_stubbed = stubs.is_some();
    sess.units_std_key = prepared.as_ref().map(|p| p.key);
    let loaded = loader::load_program(&mut sess.sm, &opts.input, load, &mut sess.diagnostics)
        .map_err(BuildError::Failed)?;
    // A load of all of std keeps std's modules for a later one ([`units_disk::save_modules`]).
    if let (Some(p), false) = (&prepared, sess.units_stubbed) {
        units_disk::save_modules(p.key, &loaded.modules, &sess.sm, opts.packages.as_ref());
    }
    sess.record("parse", t);
    Ok(loaded)
}

/// Does the build's target run on the executor in Velt (any but a wasm target)?
pub(crate) fn native_executor(opts: &BuildOptions) -> bool {
    !(opts.target.as_deref()).is_some_and(velt_codegen_llvm::is_wasm)
}

/// The prebuilt hook modules' objects a check of the program may call (`std_cache::prebuilt`):
/// none for a source generator's host, or where `scope` asks for none.
fn prebuilt_finder(
    sess: &Session,
    opts: &BuildOptions,
    scope: &CheckScope,
) -> Option<std_cache::Finder> {
    (scope.prebuilt && !sess.generator_build)
        .then(|| std_cache::Finder::for_build(opts))
        .flatten()
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
        units: scope.units,
        native_executor: native_executor(opts),
        std_key: None,
        std_stubbed: false,
        check_only: sess.check_only,
        unloaded_globals: loaded.unloaded_globals.clone(),
    };
    let packages = opts.packages.as_ref();
    // EXPERIMENT (#2943): `VELT_UNITS_TWICE` (`units_twice`).
    let mut sema_opts = sema_opts;
    if scope.units && std::env::var_os("VELT_UNITS_TWICE").is_some() {
        units_twice::run(sess, opts, loaded, &mut sema_opts, packages);
    }
    // EXPERIMENT (#2943 M2): `VELT_UNITS_DISK` (`units_disk`).
    if sess.units_std_key.is_some() {
        (sema_opts.std_key, sema_opts.std_stubbed) = (sess.units_std_key, sess.units_stubbed);
    }
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
    let mut hir = crate::generate::check(
        sess,
        &loaded.modules,
        loaded.root,
        sema_opts,
        packages,
        more.as_mut(),
    );
    sess.std_files = StdLoad::std_files(more, &loaded.modules);
    // std's modules were stubs, but no unit on disk fits this check: load std, check again.
    let needed = |d: &velt_common::Diagnostic| d.message == velt_sema::STD_UNIT_NEEDED;
    if sess.units_stubbed && sess.diagnostics.iter().any(needed) {
        sess.diagnostics.retain(|d| !needed(d));
        sess.units_no_stubs = true;
        *loaded = load_modules(sess, opts, scope)?;
        return sema_stage(sess, opts, scope, loaded);
    }
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
    let _ = t;
    sess.stop_if_errors()?;
    hir.ok_or_else(|| BuildError::Ice("sema returned no program but reported no errors".into()))
}

/// Front half of the pipeline: source file → verified VIR. Stops after the first stage with errors.
pub fn compile_to_vir(sess: &mut Session, opts: &BuildOptions) -> Result<vir::Program, BuildError> {
    compile_to_vir_linking(sess, opts, VirFor::Other)
}

/// Where the VIR [`compile_to_vir_linking`] makes goes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum VirFor {
    /// The link: std functions may be left to the std cache's objects.
    Link,
    /// The JIT in this process (`velt dev`): no link, and C variables by their own names
    /// (`LowerOptions::jit`).
    Jit,
    /// Anything else (a check, a test of the VIR): no link.
    Other,
}

/// [`compile_to_vir`] for `dest`.
fn compile_to_vir_linking(
    sess: &mut Session,
    opts: &BuildOptions,
    dest: VirFor,
) -> Result<vir::Program, BuildError> {
    let scope = CheckScope {
        require_main: true,
        extra_roots: vec![],
        eager_std: false,
        units: false,
        prebuilt: dest == VirFor::Link && opts.emit == Emit::Exe,
    };
    // EXPERIMENT (#2943 M3, `VELT_UNITS_BUILD=1` with `VELT_UNITS`): a build checks by units,
    // with every std file, as `velt check` does (`commands::check::with_all_std`).
    let by_units = std::env::var_os("VELT_UNITS_BUILD").is_some() && !sess.generator_build;
    let scope = match by_units {
        true => crate::commands::check::with_all_std(scope, opts)?,
        false => scope,
    };
    let checked = check_program(sess, opts, &scope)?;
    lower_checked(sess, opts, dest, checked)
}

/// Lowering, verification and the optimizer for a checked program (`compile_to_vir_linking`).
fn lower_checked(
    sess: &mut Session,
    opts: &BuildOptions,
    dest: VirFor,
    checked: hir::Program,
) -> Result<vir::Program, BuildError> {
    if sess.interfaces.is_some() {
        let t = Instant::now();
        sess.interfaces = Some(velt_sema::interface::Summary::of(&checked));
        sess.record("interfaces", t);
    }
    let mut hir = memory::Noted::new(Some(Box::new(checked)), "hir freed", &sess.lifetimes);

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
    // Lowering reads the source map and the counted types while the session lends its kept
    // lowering.
    let sm = std::mem::take(&mut sess.sm);
    let seed_in = sess.counted_seed.take();
    let lower_opts = velt_vir::LowerOptions {
        source_map: Some(&sm),
        std_root: std_root.as_deref(),
        native_inits: &native_inits,
        // A debug compiler links the debug runtime, which checks that no two tasks use one
        // captured variable's cell (#916); release programs and release compilers skip it. So
        // does a source generator host under either executor: it runs no tasks, so the check would
        // find nothing, and under the executor in Velt its table (std's, #2832) would reach system
        // functions (`check_generator_host`).
        cell_checks: !opts.release && cfg!(debug_assertions) && !sess.generator_build,
        // Variables for debuggers in debug builds made for debugging that keep their line
        // information (they refer to its files); release builds with `-g` keep line information
        // only, and their optimizations stay as they are.
        debug_info: !opts.release && opts.debug_vars && line_info,
        target: Some(&target),
        // Generic instances shared by shape: debug builds only, for now (shape-sharing.md).
        shapes: !opts.release,
        no_cpu_extensions: Backend::of_build(opts) == Backend::Cranelift,
        counted_seed: seed_in.as_ref(),
        jit: dest == VirFor::Jit,
        heap_hooks: heap_hooks::heap_hooks(opts.release, dest == VirFor::Jit, &target),
    };
    let mut dev_opt = None;
    let lowered = match warm::lower_kept(sess, &mut hir, &lower_opts, seed_in.is_some()) {
        Ok(Some((p, how))) => {
            dev_opt = Some(how);
            Ok(p)
        }
        Ok(None) => match hir.as_deref() {
            Some(hir) if seed_in.is_some() => {
                velt_vir::lower_seeded(hir, &lower_opts).map(|(p, seed)| {
                    sess.counted_seed = Some(seed);
                    p
                })
            }
            Some(hir) => velt_vir::lower_checked(hir, &lower_opts),
            None => return Err(BuildError::Ice("no checked program".into())),
        },
        Err(diags) => Err(diags),
    };
    sess.sm = sm;
    // A lowering that gives no counted types (a splice) keeps them.
    if sess.counted_seed.is_none() {
        sess.counted_seed = seed_in;
    }
    let mut program = match lowered {
        Ok(p) => p,
        Err(diags) => {
            sess.diagnostics.extend(diags);
            sess.stop_if_errors()?;
            return Err(BuildError::Ice("lowering reported no errors".into()));
        }
    };
    // A kept lowering holds the program it lowered.
    let kept = sess.kept_lowering.take();
    let lowered_hir = (kept.as_ref().map(|k| k.hir()))
        .or(hir.as_deref())
        .ok_or_else(|| BuildError::Ice("no checked program".into()))?;
    host_modules::check_build(sess, opts, lowered_hir, &program)?;
    sess.kept_lowering = kept;
    // Nothing after lowering reads the HIR: freed before the VIR is verified and optimized.
    drop(hir);
    if !line_info && !opts.report_numbers {
        // Panic messages already carry their locations; only debug info needs these.
        program.files.clear();
        program.funcs.iter_mut().for_each(|f| f.locs.clear());
    }
    sess.record("lower", t);

    let t = Instant::now();
    let verified = match &dev_opt {
        // The functions of the last version passed already.
        Some(warm::DevOpt::Done(changed)) => velt_vir::verify_changed(&program, changed),
        _ => velt_vir::verify(&program),
    };
    sess.record("verify", t);
    if let Err(errs) = verified {
        return Err(BuildError::Ice(format!(
            "VIR verification failed:\n  {}\n--- VIR ---\n{program}",
            errs.join("\n  ")
        )));
    }

    // The std cache (#1728), around the optimizer. A generator host stays whole:
    // `check_generator_host` looks through its code.
    let std_use = match dest == VirFor::Link && !sess.generator_build {
        true => std_cache::wanted(opts),
        false => std_cache::Use::Off,
    };
    match dev_opt {
        Some(warm::DevOpt::Done(changed)) => sess.dev_changed = Some(changed),
        Some(warm::DevOpt::Whole) => warm::optimize_kept(sess, &mut program)?,
        None => std_cache::stage::optimize(sess, opts, &target, std_use, &mut program)?,
    }
    if dest == VirFor::Link {
        std_cache::prebuilt_link(sess, opts, &target, &program)?;
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
    optimize_serving(sess, opts, program, &std::collections::HashSet::new())
}

/// [`optimize_stage`] of a program whose functions `served` names become declarations once it is
/// optimized (`std_cache::no_inline`): the optimizer leaves their bodies to the inliner
/// (`velt_opt::optimize_served`).
fn optimize_serving(
    sess: &mut Session,
    opts: &BuildOptions,
    program: &mut vir::Program,
    served: &std::collections::HashSet<String>,
) -> Result<(), BuildError> {
    let t = Instant::now();
    let level = if opts.release {
        velt_opt::OptLevel::Speed
    } else {
        velt_opt::OptLevel::None
    };
    let mut passes = velt_opt::PassTimings::new();
    velt_opt::optimize_served(program, level, &mut passes, served);
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
    on_pipeline_thread(|| {
        let built = build_on_current_thread(sess, opts);
        // `velt run` goes on to the program, which holds up no build.
        sess.turn = None;
        built
    })
}

/// Source → optimized VIR (no codegen), on a large-stack thread: for the JIT host, and for
/// `velt dev` to check a new version before replacing the running one.
pub fn compile(sess: &mut Session, opts: &BuildOptions) -> Result<vir::Program, BuildError> {
    on_pipeline_thread(|| compile_to_vir(sess, opts))
}

/// [`compile`] for the dev JIT, which runs the program in this process: what differs is how it
/// reaches C variables (`velt_vir::LowerOptions::jit`).
pub fn compile_for_jit(
    sess: &mut Session,
    opts: &BuildOptions,
) -> Result<vir::Program, BuildError> {
    on_pipeline_thread(|| compile_to_vir_linking(sess, opts, VirFor::Jit))
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

#[cfg(test)]
mod tests;
