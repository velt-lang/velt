//! `velt build` and `velt run`: single files or (without a file argument) the current package.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::backend::Backend;
use crate::cli::{BuildArgs, Emit};
use crate::driver::{self, Artifact, BuildError, BuildOptions, Session};

use super::project::Project;

/// Node's `process.argv[1]`: the script `velt run` / `velt test` runs, passed in this
/// environment variable, which the runtime reads (and removes) once at start-up.
pub(crate) const SCRIPT_VAR: &str = "VELT_SCRIPT";

/// `velt build`.
pub fn build_command(args: &BuildArgs) -> ExitCode {
    if args.json {
        return build_json(args);
    }
    match build(args) {
        Ok(Artifact::Vir(text) | Artifact::Llvm(text)) => {
            let mut out = std::io::stdout().lock();
            let _ = out.write_all(text.as_bytes());
            if !text.ends_with('\n') {
                let _ = out.write_all(b"\n");
            }
            let _ = out.flush();
            ExitCode::SUCCESS
        }
        Ok(_) => ExitCode::SUCCESS,
        Err(code) => code,
    }
}

/// `velt run`: build, then run with inherited stdio and exit with the program's exit code.
pub fn run_command(args: &BuildArgs, prog_args: &[std::ffi::OsString]) -> ExitCode {
    if let Some(target) = args.target.as_deref() {
        if let Err(msg) = runnable_here(target) {
            crate::style::error(&msg);
            return ExitCode::from(1);
        }
    }
    let (exe, script) = match build_with_input(args, false) {
        Ok((Artifact::Executable(p), input)) => {
            (vpm::relpath::absolute(&p), vpm::relpath::absolute(&input))
        }
        Ok((other, _)) => unreachable!("ICE: run built {other:?}"),
        Err(code) => return code,
    };
    let command = match args.target.as_deref().filter(|t| super::wasm::is_wasm(t)) {
        Some(target) => super::wasm::runner(target, &exe, prog_args),
        None => {
            let mut cmd = std::process::Command::new(&exe);
            cmd.args(prog_args);
            cmd.env(SCRIPT_VAR, &script);
            Ok(cmd)
        }
    };
    let mut command = match command {
        Ok(c) => c,
        Err(msg) => {
            crate::style::error(&msg);
            return ExitCode::from(1);
        }
    };
    match command.status() {
        // `process::exit` keeps the full code (Windows codes like 0xC0000005 exceed u8).
        Ok(st) => std::process::exit(exit_code(st)),
        Err(e) => {
            crate::style::error(&format!("cannot run `{}`: {e}", exe.display()));
            ExitCode::from(1)
        }
    }
}

/// Whether `velt run --target <target>` can run the program here: WebAssembly through its
/// runner, and native targets of the host's OS (another architecture may still run, e.g. under
/// Rosetta 2 or QEMU's binfmt handler; the OS decides). A program for another OS is built with
/// `velt build --target` and run there.
fn runnable_here(target: &str) -> Result<(), String> {
    if super::wasm::is_wasm(target) {
        return Ok(());
    }
    let host = velt_link::TargetOs::host();
    match velt_link::TargetOs::from_triple(target) {
        Some(os) if os == host => Ok(()),
        Some(_) => Err(format!(
            "`velt run` cannot run a `{target}` program on this machine ({}); build it with \
             `velt build --target {target}` and run it on that system",
            velt_link::host_triple()
        )),
        None => Err(format!("unsupported target `{target}`")),
    }
}

/// Resolve the build inputs (file or package), run the pipeline, print diagnostics / timings.
/// `Err` carries the process exit code.
fn build(args: &BuildArgs) -> Result<Artifact, ExitCode> {
    build_with_input(args, true).map(|(artifact, _)| artifact)
}

/// [`build`], also returning the entry source file (a package's entry when no file was given).
/// `debug_vars`: describe variables for debuggers (`BuildOptions::debug_vars`).
fn build_with_input(
    args: &BuildArgs,
    debug_vars: bool,
) -> Result<(Artifact, std::path::PathBuf), ExitCode> {
    let checked = match &args.input {
        Some(file) => super::project::check_input_file(file),
        None => Ok(()),
    };
    let mut opts = checked.and_then(|()| build_options(args)).map_err(|msg| {
        crate::style::error(&msg);
        ExitCode::from(1)
    })?;
    opts.debug_vars = debug_vars;
    let mut sess = Session::new();
    sess.show_details = args.timings;
    let result = driver::build(&mut sess, &opts);
    report(&sess, args.verbose);
    let artifact = result.map_err(|e| failure_code(&e))?;
    finish_executable(&opts, &artifact).map_err(|msg| {
        crate::style::error(&msg);
        ExitCode::from(1)
    })?;
    if opts.release && matches!(artifact, Artifact::Executable(_)) {
        warn_debug_runtime(&opts.target());
    }
    Ok((artifact, opts.input.clone()))
}

/// What a linked program needs beside it: the JavaScript glue of a WebAssembly module.
fn finish_executable(opts: &BuildOptions, artifact: &Artifact) -> Result<(), String> {
    match artifact {
        Artifact::Executable(module) => super::wasm::write_glue(&opts.target(), module),
        _ => Ok(()),
    }
}

/// Whether the build's program has line information a debugger can use: debug info was asked
/// for, and the backend emits it for the target. Cranelift writes no DWARF or CodeView into COFF
/// (Windows) objects yet, only function symbols.
fn has_line_info(opts: &BuildOptions) -> bool {
    let cranelift_coff = opts.target().contains("windows") && opts.backend != Some(Backend::Llvm);
    opts.wants_debug_info() && !cranelift_coff
}

/// `velt build --json`: `{"executable", "debugInfo", "lldbScript", "diagnostics", "errors",
/// "warnings"}` on stdout, for editors (the VS Code debugger runs the executable it names, with
/// the LLDB formatters loaded). `executable` is the absolute path of the linked program, `null`
/// when the build failed or made no program (`--emit obj|vir|llvm`); `debugInfo` says whether it
/// has line information for debuggers ([`has_line_info`]); `lldbScript` is the toolchain's
/// `velt_lldb.py` (`null` if missing). Exit codes are those of `velt build`.
fn build_json(args: &BuildArgs) -> ExitCode {
    let mut sess = Session::new();
    let checked = match &args.input {
        Some(file) => super::project::check_input_file(file),
        None => Ok(()),
    };
    let (debug_info, result) = match checked.and_then(|()| build_options(args)) {
        Ok(mut opts) => {
            // What F5 debugs.
            opts.debug_vars = true;
            let result = driver::build(&mut sess, &opts).and_then(|artifact| {
                finish_executable(&opts, &artifact).map_err(BuildError::Failed)?;
                Ok(artifact)
            });
            (Some(has_line_info(&opts)), result)
        }
        Err(msg) => (None, Err(BuildError::Failed(msg))),
    };
    let failure = match &result {
        Err(BuildError::Failed(msg)) => Some(msg.clone()),
        Err(BuildError::Ice(msg)) => Some(format!("internal compiler error: {msg}")),
        _ => None,
    };
    let mut out = super::check::report_json(&sess, failure.as_deref(), &[]);
    out["executable"] = match &result {
        Ok(Artifact::Executable(exe)) => {
            serde_json::json!(vpm::relpath::absolute(exe).to_string_lossy())
        }
        _ => serde_json::Value::Null,
    };
    out["debugInfo"] = serde_json::json!(debug_info);
    out["lldbScript"] =
        serde_json::json!(crate::debugger::lldb_script().map(|p| p.to_string_lossy().into_owned()));
    println!("{out}");
    match result {
        Ok(_) => ExitCode::SUCCESS,
        Err(BuildError::Ice(_)) => ExitCode::from(101),
        Err(_) => ExitCode::from(1),
    }
}

/// A release build linked against a debug runtime is several times slower with no other sign
/// (a debug `velt` finds the debug runtime next to it), so say so.
fn warn_debug_runtime(target: &str) {
    let Ok(runtime_lib) = velt_link::find_runtime_lib(target) else {
        return;
    };
    if velt_link::runtime_lib_is_debug(&runtime_lib) == Some(true) {
        crate::style::warning(&format!(
            "`--release` linked a debug build of the runtime ({}), which makes programs much \
             slower; use a release `velt` (`cargo build --release -p veltc -p velt_rt`) or point \
             $VELT_RT_LIB at a release runtime",
            runtime_lib.display()
        ));
    }
}

/// Print a session's diagnostics (and timings with `-v`) to stderr.
pub fn report(sess: &Session, verbose: bool) {
    let diags = sess.render_diagnostics();
    if !diags.is_empty() {
        eprintln!("{diags}");
    }
    for r in &sess.reports {
        eprint!("{r}");
    }
    if verbose {
        eprint!("{}", sess.render_timings());
    }
}

/// Print a build failure (diagnostics are already printed) and map it to an exit code.
pub fn failure_code(err: &BuildError) -> ExitCode {
    match err {
        BuildError::Diagnostics => ExitCode::from(1),
        BuildError::Failed(msg) => {
            crate::style::error(msg);
            ExitCode::from(1)
        }
        BuildError::Ice(msg) => {
            crate::style::error(&format!("internal compiler error: {msg}"));
            ExitCode::from(101)
        }
    }
}

/// Resolve `args` into build options: the file (inside its package, if any) or the current
/// package's entry, the backend, and the default output.
pub fn build_options(args: &BuildArgs) -> Result<BuildOptions, String> {
    let backend = match args.emit {
        // Printing IR needs neither clang nor a backend choice.
        Emit::Vir | Emit::Llvm => args.backend,
        // WebAssembly exists only in the LLVM backend (the driver rejects an explicit cranelift).
        Emit::Obj | Emit::Exe if args.target.as_deref().is_some_and(super::wasm::is_wasm) => {
            Some(args.backend.unwrap_or(Backend::Llvm))
        }
        Emit::Obj | Emit::Exe => {
            let (backend, note) = Backend::resolve(args.backend, args.release);
            if let Some(note) = note {
                eprintln!("{note}");
            }
            Some(backend)
        }
    };
    let mut opts = BuildOptions {
        output: args.output.clone(),
        release: args.release,
        debug_info: args.debug_info,
        target: args.target.clone(),
        emit: args.emit,
        backend,
        report_numbers: args.report_numbers,
        ..Default::default()
    };
    match &args.input {
        Some(file) => {
            let dir = file
                .parent()
                .filter(|d| !d.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            opts.packages = Project::find(dir, args.locked, &opts.target())?.map(|p| p.graph);
            opts.input = file.clone();
        }
        None => {
            let project = Project::current(vpm::InstallOptions {
                locked: args.locked,
                update: false,
                target: Some(opts.target()),
            })?;
            opts.input = project.entry()?;
            if opts.output.is_none() {
                opts.output = Some(package_output(
                    &project.target_dir(),
                    &project.manifest.package.name,
                    args.emit,
                    &opts.target(),
                ));
            }
            opts.packages = Some(project.graph);
        }
    }
    Ok(opts)
}

/// Default output of a package build: `<target dir>/<name>` (`.exe` added on Windows by the
/// driver), or `<name>.<o|obj>` for `--emit obj`.
fn package_output(target_dir: &Path, name: &str, emit: Emit, target: &str) -> PathBuf {
    match emit {
        Emit::Obj => target_dir.join(format!(
            "{name}.{}",
            if target.contains("windows") {
                "obj"
            } else {
                "o"
            }
        )),
        Emit::Exe | Emit::Vir | Emit::Llvm => target_dir.join(name),
    }
}

/// The child's exit code as our own (the low 8 bits are what the OS reports on Unix anyway);
/// signal termination on Unix maps to the shell convention 128 + signal.
pub fn exit_code(st: std::process::ExitStatus) -> i32 {
    if let Some(c) = st.code() {
        return c;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = st.signal() {
            return 128 + sig;
        }
    }
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_accepts_targets_of_this_os_only() {
        let host = velt_link::host_triple();
        assert!(runnable_here(&host).is_ok());
        assert!(runnable_here("wasm32-wasip1").is_ok());
        let other = if cfg!(windows) {
            "x86_64-unknown-linux-gnu"
        } else {
            "x86_64-pc-windows-msvc"
        };
        let err = runnable_here(other).unwrap_err();
        assert!(
            err.contains(&format!("velt build --target {other}")),
            "{err}"
        );
        assert!(runnable_here("sparc-sun-solaris").is_err());
    }

    #[test]
    fn windows_cranelift_builds_have_no_line_info() {
        let opts = |target: &str, backend, release| BuildOptions {
            target: Some(target.into()),
            backend,
            release,
            ..Default::default()
        };
        let win = "x86_64-pc-windows-msvc";
        let linux = "x86_64-unknown-linux-gnu";
        assert!(has_line_info(&opts(linux, Some(Backend::Cranelift), false)));
        assert!(!has_line_info(&opts(linux, Some(Backend::Cranelift), true)));
        assert!(!has_line_info(&opts(win, Some(Backend::Cranelift), false)));
        assert!(has_line_info(&opts(win, Some(Backend::Llvm), false)));
    }

    #[test]
    fn package_output_paths() {
        let dir = Path::new("p/target/velt");
        assert_eq!(
            package_output(dir, "app", Emit::Exe, "x86_64-pc-windows-msvc"),
            dir.join("app")
        );
        assert_eq!(
            package_output(dir, "app", Emit::Obj, "x86_64-pc-windows-msvc"),
            dir.join("app.obj")
        );
        assert_eq!(
            package_output(dir, "app", Emit::Obj, "aarch64-apple-darwin"),
            dir.join("app.o")
        );
    }
}
