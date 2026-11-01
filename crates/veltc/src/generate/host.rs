//! The generator host: the program that runs the generators a build needs (std's
//! `velt/generate` `serve` loop over them), built by the same compiler.
//!
//! It is built once into an executable in the build cache (`generate-host/<key>/host`, with its
//! own build stamp, so a host whose generators did not change is neither compiled nor linked
//! again) and run as a child process: a crash, an uncaught `throw` or `process.exit` in a
//! generator ends the host, not the compiler. Without a build cache it is JIT-compiled and run
//! by `velt __generate-host`, as `velt dev --host` runs a program.

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};

use crate::driver::{self, stamp, Artifact, BuildError, BuildOptions, Session};

use super::{guard, HOST_COMMAND};

/// The host program's source, for [`host_main`].
const ROOT_SOURCE_ENV: &str = "VELT_GENERATE_ROOT_SOURCE";

/// A host ready to run: its command, and the files its build read as build-stamp entries
/// (`file <hash> <size> <time>\t<path>`, and `dir`/`probe`/`real` for what resolving its
/// imports looked at), which the output cache and the program's own stamp
/// check (a changed generator changes the output).
pub(super) struct Host {
    command: Command,
    pub files: Vec<String>,
    /// Held while the host may run (shared): see [`executable`].
    _lock: Option<std::fs::File>,
}

/// The host for the generators `src` imports (`root`: its virtual path, which their relative
/// imports resolve from). `Err`: the host does not build, with its diagnostics as text.
pub(super) fn prepare(
    root: &Path,
    src: &str,
    packages: Option<&vpm::PackageGraph>,
) -> Result<Host, String> {
    match executable(root, src, packages) {
        Some(built) => built,
        None => Ok(Host {
            command: jit_command(root, src)?,
            files: vec![],
            _lock: None,
        }),
    }
}

/// The cached executable host: `None` without a build cache.
fn executable(
    root: &Path,
    src: &str,
    packages: Option<&vpm::PackageGraph>,
) -> Option<Result<Host, String>> {
    let dir = crate::build_cache::root()?;
    // Absolute: hosts of the same relative root in two directories import different generators.
    let root = vpm::relpath::absolute(root);
    let root = root.as_path();
    let key = velt_build_cache::Key::of(&[
        b"generate-host-v1",
        &crate::build_cache::compiler_identity().0,
        root.as_os_str().as_encoded_bytes(),
        src.as_bytes(),
    ]);
    let home = dir.join("generate-host").join(&key.hex()[..32]);
    let exe = home.join(if cfg!(windows) { "host.exe" } else { "host" });
    // Builds of the same host (two `velt` processes) take turns; a host runs under a shared
    // lock, so it is never rebuilt while it runs.
    let _ = std::fs::create_dir_all(&home);
    let lock = std::fs::File::create(home.join("lock")).ok();
    if let Some(l) = &lock {
        let _ = l.lock();
    }
    let opts = BuildOptions {
        input: root.to_path_buf(),
        root_source: Some(src.to_string()),
        output: Some(exe.clone()),
        packages: packages.cloned(),
        ..BuildOptions::default()
    };
    let mut sess = Session::new();
    sess.generator_build = true;
    let built = stamp::required(|| driver::build(&mut sess, &opts));
    let exe = match built {
        Ok(Artifact::Executable(p)) => p,
        Ok(_) => return Some(Err("the generator host is not an executable".into())),
        Err(e) => return Some(Err(report(&sess, &e))),
    };
    // What the host's build read, from its stamp (also when the stamp made the build a no-op):
    // the files, and what resolving the generators' imports looked at, so a generator module
    // that now resolves to another file (`./gen` to a new `gen.vlt` over `gen/index.vlt`) is
    // a change. The host's own objects and link inputs are not the generators' inputs.
    let text = std::fs::read_to_string(stamp::path(&exe)).unwrap_or_default();
    let files = stamp::entry_lines(&text)
        .into_iter()
        .filter(|l| {
            ["file ", "dir ", "probe ", "real "]
                .iter()
                .any(|k| l.starts_with(k))
        })
        .map(String::from)
        .collect();
    if let Some(l) = &lock {
        let _ = l.unlock();
        let _ = l.lock_shared();
    }
    Some(Ok(Host {
        command: Command::new(exe),
        files,
        _lock: lock,
    }))
}

fn jit_command(root: &Path, src: &str) -> Result<Command, String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot find `velt`: {e}"))?;
    let mut cmd = Command::new(exe);
    cmd.arg(HOST_COMMAND).arg(root).env(ROOT_SOURCE_ENV, src);
    guard::pass_depth(&mut cmd);
    Ok(cmd)
}

impl Host {
    /// Run the host on `input` (the requests) and return its standard output.
    pub fn run(mut self, input: &str) -> Result<Vec<u8>, String> {
        let mut child = (self.command)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("cannot start the generator host: {e}"))?;
        let mut stdin = child.stdin.take().ok_or("no stdin")?;
        let input = input.to_string();
        let writer = std::thread::spawn(move || {
            let _ = stdin.write_all(input.as_bytes());
        });
        let mut out = vec![];
        let mut err = vec![];
        let mut stdout = child.stdout.take().ok_or("no stdout")?;
        let mut stderr = child.stderr.take().ok_or("no stderr")?;
        let errs = std::thread::spawn(move || {
            let _ = stderr.read_to_end(&mut err);
            err
        });
        let _ = stdout.read_to_end(&mut out);
        let status = child.wait().map_err(|e| e.to_string())?;
        let _ = writer.join();
        let err = errs.join().unwrap_or_default();
        if !status.success() {
            let err = String::from_utf8_lossy(&err);
            return Err(format!(
                "the generator host exited with {status}:\n{}",
                err.trim_end()
            ));
        }
        Ok(out)
    }
}

/// `velt __generate-host <virtual root path>`: build the host program (its source in
/// [`ROOT_SOURCE_ENV`]) with the JIT and run it; exits with its exit code.
pub fn host_main(root: &Path) -> ExitCode {
    let Ok(src) = std::env::var(ROOT_SOURCE_ENV) else {
        crate::style::error("velt __generate-host is run by the compiler");
        return ExitCode::from(2);
    };
    let opts = BuildOptions {
        input: root.to_path_buf(),
        root_source: Some(src),
        ..BuildOptions::default()
    };
    let mut sess = Session::new();
    sess.generator_build = true;
    let symbols = velt_rt_host::abi_symbols::symbol_table();
    let mut session = velt_codegen_cl::DevSession::new(&symbols);
    session.set_debug_info(false);
    let program = match driver::compile(&mut sess, &opts)
        .and_then(|p| driver::check_generator_host(&mut sess, &p).map(|()| p))
    {
        Ok(p) => p,
        Err(e) => {
            eprint!("{}", report(&sess, &e));
            return ExitCode::from(1);
        }
    };
    let loaded = match session.load_timed(&program, &mut vec![]) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("code generation failed: {e}");
            return ExitCode::from(1);
        }
    };
    velt_rt_host::process::set_args(vec![root.display().to_string()]);
    let code = velt_rt_host::entry::run_main(loaded.main());
    std::process::exit(code)
}

fn report(sess: &Session, e: &BuildError) -> String {
    let mut out = String::new();
    for d in &sess.diagnostics {
        out.push_str(&d.render(&sess.sm));
        out.push('\n');
    }
    match e {
        BuildError::Diagnostics => {}
        BuildError::Failed(m) | BuildError::Ice(m) => out.push_str(&format!("error: {m}\n")),
    }
    out
}
