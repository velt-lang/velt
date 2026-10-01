//! `velt build` and `velt run`: single files or (without a file argument) the current package.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::backend::Backend;
use crate::cli::{BuildArgs, Emit};
use crate::driver::{self, Artifact, BuildError, BuildOptions, Session};

use super::project::Project;

/// `velt build`.
pub fn build_command(args: &BuildArgs) -> ExitCode {
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
    let exe = match build(args) {
        Ok(Artifact::Executable(p)) => vpm::relpath::absolute(&p),
        Ok(other) => unreachable!("ICE: run built {other:?}"),
        Err(code) => return code,
    };
    let command = match args.target.as_deref().filter(|t| super::wasm::is_wasm(t)) {
        Some(target) => super::wasm::runner(target, &exe, prog_args),
        None => {
            let mut cmd = std::process::Command::new(&exe);
            cmd.args(prog_args);
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

/// Resolve the build inputs (file or package), run the pipeline, print diagnostics / timings.
/// `Err` carries the process exit code.
fn build(args: &BuildArgs) -> Result<Artifact, ExitCode> {
    let checked = match &args.input {
        Some(file) => super::project::check_input_file(file),
        None => Ok(()),
    };
    let opts = checked.and_then(|()| build_options(args)).map_err(|msg| {
        crate::style::error(&msg);
        ExitCode::from(1)
    })?;
    let mut sess = Session::new();
    sess.show_details = args.timings;
    let result = driver::build(&mut sess, &opts);
    report(&sess, args.verbose);
    let artifact = result.map_err(|e| failure_code(&e))?;
    if let Artifact::Executable(module) = &artifact {
        super::wasm::write_glue(&opts.target(), module).map_err(|msg| {
            crate::style::error(&msg);
            ExitCode::from(1)
        })?;
        if opts.release {
            warn_debug_runtime(&opts.target());
        }
    }
    Ok(artifact)
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
