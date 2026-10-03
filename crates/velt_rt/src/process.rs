//! Process facilities: `process.argv` (and `argv()` / `args()`), `process.env`, `process.cwd()`/`chdir`, `performance.now()`
//! and `Date.now()`. (`process.exit` is `velt_rt_exit` in panic.rs.)
//!
//! Arguments and environment values are converted to UTF-8 from the OS representation (UTF-16 wide
//! APIs on Windows via `std::env::args_os`/`var_os`); unpaired surrogates / invalid bytes become
//! U+FFFD.

use crate::result::{IoResult, VeltErr};
use crate::str::VeltStr;
use crate::str_array::VeltStrArray;
use std::sync::OnceLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

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
    let args = all_args();
    let exe = std::env::current_exe()
        .ok()
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

/// Fix the `performance.now()` time origin (process start). Idempotent.
pub fn init_clock() {
    ORIGIN.get_or_init(Instant::now);
}

unsafe fn text<'a>(s: *const VeltStr) -> std::borrow::Cow<'a, str> {
    String::from_utf8_lossy((*s).as_bytes())
}

/// `argv()` of `velt:process`: all arguments including the program path, as owned UTF-8
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

/// `process.env[name]`: returns 1 and writes an owned string to `out` if set, else returns 0 and
/// leaves `out` untouched.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_env_get(name: *const VeltStr, out: *mut VeltStr) -> u8 {
    match std::env::var_os(&*text(name)) {
        Some(v) => {
            out.write(VeltStr::from_vec(
                v.to_string_lossy().into_owned().into_bytes(),
            ));
            1
        }
        None => 0,
    }
}

/// `envAll()`: every environment variable as `[name0, value0, name1, value1, …]`, in the order
/// the OS keeps them (Node's `process.env` order). Windows' per-drive `=C:` entries are left
/// out, as Node does; names and values that are not Unicode are converted lossily.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_env_all(out: *mut VeltStrArray) {
    let pairs = std::env::vars_os()
        .map(|(k, v)| {
            (
                k.to_string_lossy().into_owned(),
                v.to_string_lossy().into_owned(),
            )
        })
        .filter(|(k, _)| !k.is_empty() && !k.starts_with('='))
        .flat_map(|(k, v)| [k, v]);
    out.write(VeltStrArray::from_strings(pairs));
}

/// `process.env[name] = value`. Not synchronized with other threads reading the C environment
/// (a platform limitation); set variables before starting concurrent work.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_env_set(name: *const VeltStr, value: *const VeltStr) {
    std::env::set_var(&*text(name), &*text(value));
}

/// `delete process.env[name]`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_env_remove(name: *const VeltStr) {
    std::env::remove_var(&*text(name));
}

/// `process.cwd()` into `out` (no return value; std reads the code from `out`).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_process_cwd(out: *mut IoResult<VeltStr>) {
    let r = std::env::current_dir().map(|p| p.to_string_lossy().into_owned());
    IoResult::from_io(r, |s| VeltStr::from_vec(s.into_bytes())).write_to(out);
}

/// `process.chdir(path)`; `out` receives the status.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_process_chdir(path: *const VeltStr, out: *mut VeltErr) {
    let e = match std::env::set_current_dir(&*text(path)) {
        Ok(()) => VeltErr::ok(),
        Err(e) => VeltErr::from_io(&e),
    };
    out.write(e);
}

/// `performance.now()`: monotonic milliseconds (fractional) since process start.
#[no_mangle]
pub extern "C" fn velt_rt_perf_now() -> f64 {
    ORIGIN.get_or_init(Instant::now).elapsed().as_secs_f64() * 1000.0
}

/// `Date.now()`: whole milliseconds since the Unix epoch.
#[no_mangle]
pub extern "C" fn velt_rt_date_now() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_millis() as i64,
        Err(e) => -(e.duration().as_millis() as i64),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::MaybeUninit;

    #[test]
    fn args_env_clock() {
        let mut a = MaybeUninit::<VeltStrArray>::uninit();
        unsafe { velt_rt_process_args(a.as_mut_ptr()) };
        let mut a = unsafe { a.assume_init() };
        assert!(a.len >= 1);
        unsafe { crate::str_array::velt_rt_str_array_drop(&mut a) };

        let name = VeltStr::from_static(b"VELT_RT_TEST_ENV_\xc3\xa9");
        let value = VeltStr::from_static("värde".as_bytes());
        let mut out = MaybeUninit::<VeltStr>::uninit();
        unsafe { velt_rt_env_set(&name, &value) };
        assert_eq!(unsafe { velt_rt_env_get(&name, out.as_mut_ptr()) }, 1);
        let mut got = unsafe { out.assume_init() };
        assert_eq!(unsafe { got.as_bytes() }, "värde".as_bytes());
        unsafe { crate::str::velt_rt_str_drop(&mut got) };
        let mut all = MaybeUninit::<VeltStrArray>::uninit();
        unsafe { velt_rt_env_all(all.as_mut_ptr()) };
        let mut all = unsafe { all.assume_init() };
        let pairs: Vec<String> = (0..all.len as usize)
            .map(|i| String::from_utf8_lossy(unsafe { (*all.ptr.add(i)).as_bytes() }).into_owned())
            .collect();
        assert_eq!(pairs.len() % 2, 0);
        let at = pairs
            .iter()
            .step_by(2)
            .position(|k| k == "VELT_RT_TEST_ENV_é")
            .expect("the variable just set is listed");
        assert_eq!(pairs[2 * at + 1], "värde");
        assert!(pairs.iter().step_by(2).all(|k| !k.starts_with('=')));
        unsafe { crate::str_array::velt_rt_str_array_drop(&mut all) };
        unsafe { velt_rt_env_remove(&name) };
        let mut absent = MaybeUninit::<VeltStr>::uninit();
        assert_eq!(unsafe { velt_rt_env_get(&name, absent.as_mut_ptr()) }, 0);

        let t0 = velt_rt_perf_now();
        std::thread::sleep(std::time::Duration::from_millis(5));
        assert!(velt_rt_perf_now() - t0 >= 4.0);
        assert!(velt_rt_date_now() > 1_600_000_000_000);

        let mut cwd = MaybeUninit::<IoResult<VeltStr>>::uninit();
        unsafe { velt_rt_process_cwd(cwd.as_mut_ptr()) };
        let cwd = unsafe { cwd.assume_init() };
        assert_eq!(cwd.err.code, 0);
        let mut s = unsafe { cwd.value.assume_init() };
        assert!(!s.is_empty());
        unsafe { crate::str::velt_rt_str_drop(&mut s) };
    }
}
