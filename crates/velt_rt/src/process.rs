//! Process facilities: `process.argv` (and `argv()` / `args()`) and the time origin of the
//! runtime's timers. (`process.exit` is `velt_rt_exit` in panic.rs.)
//!
//! Arguments and environment values are converted to UTF-8 from the OS representation (UTF-16 wide
//! APIs on Windows via `std::env::args_os`/`var_os`); unpaired surrogates / invalid bytes become
//! U+FFFD.

use crate::str_array::VeltStrArray;
use std::sync::OnceLock;
use std::time::Instant;

static ORIGIN: OnceLock<Instant> = OnceLock::new();
/// `process.argv` when set by an embedder (the `velt dev` JIT host runs inside `velt`, whose own
/// arguments are not the program's).
static ARGS: OnceLock<Vec<String>> = OnceLock::new();

/// The script `velt run` / `velt dev` runs (Node's `process.argv[1]`): set by the JIT host, or
/// taken from `$VELT_SCRIPT` at start-up (and removed there, so child processes don't inherit
/// it). `None`: a compiled program run directly, whose `argv[1]` is the executable.
static SCRIPT: OnceLock<Option<String>> = OnceLock::new();

/// The environment variable `velt run` passes the script's source path in.
pub const SCRIPT_VAR: &str = "VELT_SCRIPT";

/// Replace `process.argv` (program path first). Only the first call has an effect.
pub fn set_args(args: Vec<String>) {
    let _ = ARGS.set(args);
}

/// Set the script path of Node's `process.argv[1]` (the JIT host). Only the first call (or
/// [`init_script`]) has an effect.
pub fn set_script(path: String) {
    let _ = SCRIPT.set(Some(path));
}

/// Read and remove `$VELT_SCRIPT` (start-up, before the program can start a child process).
pub fn init_script() {
    SCRIPT.get_or_init(|| {
        let path = std::env::var_os(SCRIPT_VAR)?;
        std::env::remove_var(SCRIPT_VAR);
        Some(path.to_string_lossy().into_owned())
    });
}

/// Every argument, program path first.
fn all_args() -> Vec<String> {
    match ARGS.get() {
        Some(args) => args.clone(),
        None => std::env::args_os()
            .map(|a| a.to_string_lossy().into_owned())
            .collect(),
    }
}

/// Node's `process.argv`: `[runtime, script, ...args]`. The runtime is the running executable
/// (the program, or `velt` under `velt dev`); the script is the source file under `velt run` /
/// `velt dev`, else the executable again (as for a Node single-executable application).
pub fn node_argv() -> Vec<String> {
    static NODE_ARGV: OnceLock<Vec<String>> = OnceLock::new();
    NODE_ARGV.get_or_init(build_node_argv).clone()
}

fn build_node_argv() -> Vec<String> {
    let args = all_args();
    let exe = executable()
        .map(|p| p.to_string_lossy().into_owned())
        .or_else(|| args.first().cloned())
        .unwrap_or_default();
    let script = SCRIPT
        .get()
        .cloned()
        .flatten()
        .unwrap_or_else(|| exe.clone());
    let mut out = vec![exe, script];
    out.extend(args.into_iter().skip(1));
    out
}

/// The running executable's resolved path, as Node's `process.execPath`: on macOS
/// `current_exe` may be the symlink the program was started through, and on Linux it ends in
/// ` (deleted)` once the file was replaced or removed while running. Windows paths stay as they
/// are (canonicalizing would turn them into `\?\` paths).
fn executable() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    #[cfg(target_os = "linux")]
    let exe = without_deleted_suffix(exe);
    #[cfg(not(windows))]
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    Some(exe)
}

/// `/proc/self/exe` of a replaced binary: `/path/prog (deleted)` becomes `/path/prog`, the path
/// the program was started as.
#[cfg(any(target_os = "linux", test))]
fn without_deleted_suffix(exe: std::path::PathBuf) -> std::path::PathBuf {
    match exe.to_str().and_then(|s| s.strip_suffix(" (deleted)")) {
        Some(stripped) if !exe.exists() => std::path::PathBuf::from(stripped),
        _ => exe,
    }
}

/// Fix the time origin of the runtime's timers (process start). Idempotent.
pub fn init_clock() {
    ORIGIN.get_or_init(Instant::now);
}

/// `argv()` of `velt/process`: all arguments including the program path, as owned UTF-8
/// strings.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_process_args(out: *mut VeltStrArray) {
    out.write(VeltStrArray::from_strings(all_args()));
}

/// Node's `process.argv` ([`node_argv`]).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_process_node_argv(out: *mut VeltStrArray) {
    out.write(VeltStrArray::from_strings(node_argv()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::MaybeUninit;

    #[test]
    fn args_and_clock() {
        let mut a = MaybeUninit::<VeltStrArray>::uninit();
        unsafe { velt_rt_process_args(a.as_mut_ptr()) };
        let mut a = unsafe { a.assume_init() };
        assert!(a.len >= 1);
        unsafe { crate::str_array::velt_rt_str_array_drop(&mut a) };
    }

    #[test]
    fn executable_paths_are_resolved() {
        let exe = executable().expect("the test executable");
        assert!(exe.is_absolute() && exe.exists(), "{}", exe.display());
        // A replaced binary's `/proc/self/exe` loses its ` (deleted)` marker.
        let gone = std::env::temp_dir().join("velt-no-such-program (deleted)");
        assert_eq!(
            without_deleted_suffix(gone.clone()),
            std::env::temp_dir().join("velt-no-such-program")
        );
        // Other paths stay as they are.
        let real = std::env::current_exe().unwrap();
        assert_eq!(without_deleted_suffix(real.clone()), real);
    }
}
