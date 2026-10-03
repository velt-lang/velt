//! Process state (rt_abi_async.md §8): arguments, environment, working directory and clocks.
//! Same behavior as velt_rt's `process` module; the clocks go through [`crate::platform`] so
//! they also work in the browser, where `std`'s clocks are unavailable.

use crate::platform;
use crate::result::{IoResult, VeltErr};
use crate::str::VeltStr;
use crate::str_array::VeltStrArray;

unsafe fn text<'a>(s: *const VeltStr) -> std::borrow::Cow<'a, str> {
    String::from_utf8_lossy((*s).as_bytes())
}

/// `process.argv`: all arguments including the program path.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_process_args(out: *mut VeltStrArray) {
    out.write(VeltStrArray::from_strings(platform::args()));
}

/// Node's `process.argv`: `[program, program, ...args]` (WebAssembly has no separate runtime
/// path or script; the browser passes no arguments).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_process_node_argv(out: *mut VeltStrArray) {
    let args = platform::args();
    let program = args.first().cloned().unwrap_or_default();
    let mut all = vec![program.clone(), program];
    all.extend(args.into_iter().skip(1));
    out.write(VeltStrArray::from_strings(all));
}

/// `process.env[name]`: 1 and an owned string in `out` if set, else 0 (`out` untouched).
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

/// `process.env[name] = value`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_env_set(name: *const VeltStr, value: *const VeltStr) {
    std::env::set_var(&*text(name), &*text(value));
}

/// `delete process.env[name]`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_env_remove(name: *const VeltStr) {
    std::env::remove_var(&*text(name));
}

/// `process.cwd()` into `out` (as in velt_rt).
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

/// `performance.now()`: monotonic milliseconds since the first clock use.
#[no_mangle]
pub extern "C" fn velt_rt_perf_now() -> f64 {
    platform::monotonic_ms()
}

/// `Date.now()`: whole milliseconds since the Unix epoch.
#[no_mangle]
pub extern "C" fn velt_rt_date_now() -> i64 {
    platform::epoch_ms()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::MaybeUninit;

    #[test]
    fn args_env_clock() {
        let mut args = MaybeUninit::<VeltStrArray>::uninit();
        unsafe { velt_rt_process_args(args.as_mut_ptr()) };
        let mut args = unsafe { args.assume_init() };
        assert!(args.len >= 1);
        unsafe { crate::str_array::velt_rt_str_array_drop(&mut args) };

        let name = VeltStr::from_static(b"VELT_RT_WASM_TEST_ENV");
        let value = VeltStr::from_static(b"yes");
        let mut out = VeltStr::empty();
        unsafe {
            velt_rt_env_set(&name, &value);
            assert_eq!(velt_rt_env_get(&name, &mut out), 1);
            assert_eq!(out.as_bytes(), b"yes");
            crate::str::velt_rt_str_drop(&mut out);
        }
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
            .position(|k| k == "VELT_RT_WASM_TEST_ENV")
            .expect("the variable just set is listed");
        assert_eq!(pairs[2 * at + 1], "yes");
        assert!(pairs.iter().step_by(2).all(|k| !k.starts_with('=')));
        unsafe { crate::str_array::velt_rt_str_array_drop(&mut all) };
        unsafe {
            velt_rt_env_remove(&name);
            assert_eq!(velt_rt_env_get(&name, &mut out), 0);
        }
        assert!(velt_rt_perf_now() >= 0.0);
        assert!(velt_rt_date_now() > 1_600_000_000_000);
    }
}
