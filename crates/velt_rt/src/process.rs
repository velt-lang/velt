//! Process facilities: `process.argv`, `process.env`, `process.cwd()`/`chdir`, `performance.now()`
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

/// Replace `process.argv` (program path first). Only the first call has an effect.
pub fn set_args(args: Vec<String>) {
    let _ = ARGS.set(args);
}

/// Fix the `performance.now()` time origin (process start). Idempotent.
pub fn init_clock() {
    ORIGIN.get_or_init(Instant::now);
}

unsafe fn text<'a>(s: *const VeltStr) -> std::borrow::Cow<'a, str> {
    String::from_utf8_lossy((*s).as_bytes())
}

/// `process.argv`: all arguments including the program path, as owned UTF-8 strings.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_process_args(out: *mut VeltStrArray) {
    let array = match ARGS.get() {
        Some(args) => VeltStrArray::from_strings(args.iter().cloned()),
        None => VeltStrArray::from_strings(
            std::env::args_os().map(|a| a.to_string_lossy().into_owned()),
        ),
    };
    out.write(array);
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
