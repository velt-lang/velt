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

/// Stack for the pipeline thread (reserved, not committed, until used).
const PIPELINE_STACK_BYTES: usize = 256 << 20;

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
    /// What to produce.
    pub emit: Emit,
    /// Code generator; `None` → [`Backend::resolve`] (LLVM for release builds when clang exists).
    pub backend: Option<Backend>,
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
                "velt: {name:<8} {:>9.3} ms\n",
                d.as_secs_f64() * 1e3
            ));
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

    fn record_details(&mut self, stage: &'static str, steps: &[(&'static str, Duration)]) {
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
/// first stage with errors.
fn check_program(sess: &mut Session, opts: &BuildOptions) -> Result<hir::Program, BuildError> {
    let t = Instant::now();
    let load = LoadOptions {
        std_root: loader::std_root(),
        packages: opts
            .packages
            .as_ref()
            .map(|g| g as &dyn loader::PackageResolver),
        root_source: opts.root_source.clone(),
        overlay: None,
    };
    let loaded = loader::load_program(&mut sess.sm, &opts.input, load, &mut sess.diagnostics)
        .map_err(BuildError::Failed)?;
    sess.record("parse", t);
    sess.stop_if_errors()?;

    let t = Instant::now();
    let (hir, diags) = velt_sema::check(&loaded.modules, loaded.root);
    sess.diagnostics.extend(diags);
    if let (Some(hir), Some(graph)) = (&hir, &opts.packages) {
        let std_root = loader::std_root();
        crate::native::check_declares(
            hir,
            &sess.sm,
            std_root.as_deref(),
            graph,
            &mut sess.diagnostics,
        );
    }
    sess.record("sema", t);
    sess.stop_if_errors()?;
    hir.ok_or_else(|| BuildError::Ice("sema returned no program but reported no errors".into()))
}

/// Front half of the pipeline: source file → verified VIR. Stops after the first stage with errors.
pub fn compile_to_vir(sess: &mut Session, opts: &BuildOptions) -> Result<vir::Program, BuildError> {
    let hir = check_program(sess, opts)?;

    let t = Instant::now();
    let std_root = loader::std_root();
    let native_inits = crate::native::inits(opts.packages.as_ref());
    let lower_opts = velt_vir::LowerOptions {
        source_map: Some(&sess.sm),
        std_root: std_root.as_deref(),
        native_inits: &native_inits,
    };
    let mut program = velt_vir::lower_with(&hir, &lower_opts);
    if !opts.wants_debug_info() {
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

    let t = Instant::now();
    let level = if opts.release {
        velt_opt::OptLevel::Speed
    } else {
        velt_opt::OptLevel::None
    };
    let mut passes = velt_opt::PassTimings::new();
    velt_opt::optimize_timed(&mut program, level, &mut passes);
    let verify_start = Instant::now();
    let verified = velt_vir::verify(&program);
    let mut steps = passes.entries().to_vec();
    steps.push(("verify", verify_start.elapsed()));
    sess.record("optimize", t);
    sess.record_details("optimize", &steps);
    if let Err(errs) = verified {
        return Err(BuildError::Ice(format!(
            "VIR verification failed after optimization:\n  {}",
            errs.join("\n  ")
        )));
    }
    Ok(program)
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
/// reports, no lowering, codegen or link.
pub fn check(sess: &mut Session, opts: &BuildOptions) -> Result<(), BuildError> {
    on_pipeline_thread(|| check_program(sess, opts).map(drop))
}

/// Run a pipeline stage on the dedicated large-stack thread.
fn on_pipeline_thread<T: Send>(
    stage: impl FnOnce() -> Result<T, BuildError> + Send,
) -> Result<T, BuildError> {
    std::thread::scope(|s| {
        let spawned = std::thread::Builder::new()
            .name("velt-compile".into())
            .stack_size(PIPELINE_STACK_BYTES)
            .spawn_scoped(s, stage);
        match spawned {
            Ok(handle) => handle
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
            Err(e) => Err(BuildError::Failed(format!(
                "cannot start the compiler thread: {e}"
            ))),
        }
    })
}

fn build_on_current_thread(
    sess: &mut Session,
    opts: &BuildOptions,
) -> Result<Artifact, BuildError> {
    let target = opts.target();
    let program = compile_to_vir(sess, opts)?;
    match opts.emit {
        Emit::Vir => return Ok(Artifact::Vir(program.to_string())),
        Emit::Llvm => {
            return velt_codegen_llvm::emit_ir(&program, &target)
                .map(Artifact::Llvm)
                .map_err(|e| BuildError::Failed(format!("code generation failed: {e}")))
        }
        Emit::Obj | Emit::Exe => {}
    }

    let paths = OutputPaths::new(&opts.input, opts.output.as_deref(), opts.emit, &target);

    let t = Instant::now();
    let cg = velt_codegen_cl::CodegenOptions {
        target: target.clone(),
        optimize: opts.release,
    };
    let (backend, _) = Backend::resolve(opts.backend, opts.release);
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
    let objects = backend
        .emit_objects(&program, &cg, units, &mut steps)
        .map_err(|e| BuildError::Failed(format!("code generation failed: {e}")))?;
    sess.record("codegen", t);
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

    let t = Instant::now();
    let linked = crate::link::link_executable(
        &target,
        &object_paths,
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
    Ok(Artifact::Executable(paths.executable))
}

/// `$VELT_CODEGEN_UNITS`: how many codegen units the LLVM backend splits a program into, at most
/// the core count (unset or not a positive number: one).
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

/// Where `build` puts its outputs (relative paths are relative to the current directory).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputPaths {
    /// Linked executable (empty for `--emit obj`).
    pub executable: PathBuf,
    /// Object file.
    pub object: PathBuf,
}

impl OutputPaths {
    /// Object file of codegen unit `i`: [`Self::object`] for the first, `<stem>.cgu<i>.<ext>`
    /// beside it for the others.
    pub fn unit_object(&self, i: usize) -> PathBuf {
        if i == 0 {
            return self.object.clone();
        }
        let stem = self
            .object
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy();
        match self.object.extension() {
            Some(ext) => self
                .object
                .with_file_name(format!("{stem}.cgu{i}.{}", ext.to_string_lossy())),
            None => self.object.with_file_name(format!("{stem}.cgu{i}")),
        }
    }

    /// Removes the objects of codegen units `count` and above, left by an earlier build with more
    /// units (so a directory listing or a manual link doesn't pick up stale code).
    pub fn remove_unit_objects_from(&self, count: usize) {
        for i in count.max(1).. {
            let path = self.unit_object(i);
            if std::fs::remove_file(&path).is_err() {
                break;
            }
        }
    }

    /// Output paths for `input` given `-o`, `--emit` and the target.
    pub fn new(input: &Path, output: Option<&Path>, emit: Emit, target: &str) -> OutputPaths {
        if velt_codegen_llvm::is_wasm(target) {
            return Self::wasm(input, output, emit);
        }
        let windows = target.contains("windows");
        let obj_ext = if windows { "obj" } else { "o" };
        let stem = input
            .file_stem()
            .map_or_else(|| "main".into(), |s| s.to_string_lossy().into_owned());

        if emit == Emit::Obj {
            let object = match output {
                Some(o) => o.to_path_buf(),
                None => Path::new("target")
                    .join("velt")
                    .join(format!("{stem}.{obj_ext}")),
            };
            return OutputPaths {
                executable: PathBuf::new(),
                object,
            };
        }

        let executable = match output {
            // `link.exe /OUT:foo` would produce an unrunnable `foo`; Windows executables need `.exe`.
            Some(o) if windows && o.extension().is_none() => o.with_extension("exe"),
            Some(o) => o.to_path_buf(),
            None => Path::new("target").join("velt").join(if windows {
                format!("{stem}.exe")
            } else {
                stem
            }),
        };
        // Object next to the executable: `<exe name without .exe>.<obj_ext>`.
        let name = executable
            .file_name()
            .map_or_else(|| "main".into(), |s| s.to_string_lossy().into_owned());
        let base = if windows {
            name.strip_suffix(".exe").unwrap_or(&name)
        } else {
            &name
        };
        let object = executable.with_file_name(format!("{base}.{obj_ext}"));
        OutputPaths { executable, object }
    }

    /// WebAssembly outputs: `<stem>.wasm` (a module, not an executable) and `<stem>.o`.
    fn wasm(input: &Path, output: Option<&Path>, emit: Emit) -> OutputPaths {
        let stem = input
            .file_stem()
            .map_or_else(|| "main".into(), |s| s.to_string_lossy().into_owned());
        let default_dir = Path::new("target").join("velt");
        if emit == Emit::Obj {
            let object =
                output.map_or_else(|| default_dir.join(format!("{stem}.o")), Path::to_path_buf);
            return OutputPaths {
                executable: PathBuf::new(),
                object,
            };
        }
        let executable = match output {
            Some(o) if o.extension().is_none() => o.with_extension("wasm"),
            Some(o) => o.to_path_buf(),
            None => default_dir.join(format!("{stem}.wasm")),
        };
        let object = executable.with_extension("o");
        OutputPaths { executable, object }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIN: &str = "x86_64-pc-windows-msvc";
    const LINUX: &str = "x86_64-unknown-linux-gnu";

    fn paths(input: &str, out: Option<&str>, emit: Emit, target: &str) -> (PathBuf, PathBuf) {
        let p = OutputPaths::new(Path::new(input), out.map(Path::new), emit, target);
        (p.executable, p.object)
    }

    #[test]
    fn codegen_unit_objects_sit_beside_the_first() {
        let p = OutputPaths::new(
            Path::new("app.vlt"),
            Some(Path::new("out/app")),
            Emit::Exe,
            LINUX,
        );
        assert_eq!(p.unit_object(0), PathBuf::from("out/app.o"));
        assert_eq!(p.unit_object(2), PathBuf::from("out/app.cgu2.o"));
        let p = OutputPaths::new(Path::new("app.vlt"), None, Emit::Exe, WIN);
        let dir = Path::new("target").join("velt");
        assert_eq!(p.unit_object(1), dir.join("app.cgu1.obj"));
    }

    #[test]
    fn fewer_codegen_units_remove_stale_objects() {
        let dir = tempfile::tempdir().unwrap();
        let p = OutputPaths::new(
            Path::new("app.vlt"),
            Some(&dir.path().join("app")),
            Emit::Exe,
            LINUX,
        );
        for i in 0..4 {
            std::fs::write(p.unit_object(i), b"obj").unwrap();
        }
        p.remove_unit_objects_from(2);
        let exists: Vec<bool> = (0..4).map(|i| p.unit_object(i).exists()).collect();
        assert_eq!(exists, [true, true, false, false]);
        p.remove_unit_objects_from(1);
        assert!(p.unit_object(0).exists() && !p.unit_object(1).exists());
    }

    #[test]
    fn default_output_paths() {
        let dir = Path::new("target").join("velt");
        assert_eq!(
            paths("tests/hello.vlt", None, Emit::Exe, WIN),
            (dir.join("hello.exe"), dir.join("hello.obj"))
        );
        assert_eq!(
            paths("hello.vlt", None, Emit::Exe, LINUX),
            (dir.join("hello"), dir.join("hello.o"))
        );
        assert_eq!(
            paths("a.b.vlt", None, Emit::Exe, LINUX),
            (dir.join("a.b"), dir.join("a.b.o"))
        );
        assert_eq!(
            paths("hello.vlt", None, Emit::Obj, LINUX).1,
            dir.join("hello.o")
        );
    }

    #[test]
    fn explicit_output_paths() {
        assert_eq!(
            paths("h.vlt", Some("out/app"), Emit::Exe, WIN),
            (PathBuf::from("out/app.exe"), PathBuf::from("out/app.obj"))
        );
        assert_eq!(
            paths("h.vlt", Some("out/app.exe"), Emit::Exe, WIN).1,
            PathBuf::from("out/app.obj")
        );
        assert_eq!(
            paths("h.vlt", Some("out/app"), Emit::Exe, LINUX),
            (PathBuf::from("out/app"), PathBuf::from("out/app.o"))
        );
        assert_eq!(
            paths("h.vlt", Some("x.o"), Emit::Obj, LINUX).1,
            PathBuf::from("x.o")
        );
    }

    #[test]
    fn wasm_output_paths() {
        let dir = Path::new("target").join("velt");
        assert_eq!(
            paths("hi.vlt", None, Emit::Exe, "wasm32-wasip1"),
            (dir.join("hi.wasm"), dir.join("hi.o"))
        );
        assert_eq!(
            paths(
                "hi.vlt",
                Some("out/app"),
                Emit::Exe,
                "wasm32-unknown-unknown"
            ),
            (PathBuf::from("out/app.wasm"), PathBuf::from("out/app.o"))
        );
        assert_eq!(
            paths("hi.vlt", None, Emit::Obj, "wasm32-wasip1").1,
            dir.join("hi.o")
        );
    }

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
