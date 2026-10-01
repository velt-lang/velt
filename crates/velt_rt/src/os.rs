//! `std/os`: facts about the machine and platform (Node's `os` module subset). Everything else
//! std needs about the OS (environment, cwd) is in `process.rs`.

use crate::str::VeltStr;

/// Node's `process.platform` names: `darwin`, `linux`, `win32`, else Rust's `std::env::consts::OS`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_os_platform(out: *mut VeltStr) {
    let name = match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    };
    out.write(VeltStr::from_static(name.as_bytes()));
}

/// Node's `process.arch` names: `x64`, `arm64`, else Rust's `std::env::consts::ARCH`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_os_arch(out: *mut VeltStr) {
    let name = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => other,
    };
    out.write(VeltStr::from_static(name.as_bytes()));
}

/// Logical CPUs available to this process (`os.availableParallelism()`); at least 1.
#[no_mangle]
pub extern "C" fn velt_rt_os_cpu_count() -> u64 {
    std::thread::available_parallelism().map_or(1, |n| n.get() as u64)
}

/// The directory for temporary files (`os.tmpdir()`), without a trailing separator.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_os_tmpdir(out: *mut VeltStr) {
    let dir = std::env::temp_dir().to_string_lossy().into_owned();
    let trimmed = if dir.len() > 1 {
        dir.trim_end_matches(['/', '\\'])
    } else {
        &dir
    };
    out.write(VeltStr::from_vec(trimmed.as_bytes().to_vec()));
}

/// The host name (`os.hostname()`); empty if unknown.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_os_hostname(out: *mut VeltStr) {
    out.write(VeltStr::from_vec(hostname().into_bytes()));
}

#[cfg(unix)]
fn hostname() -> String {
    let mut buf = [0u8; 256];
    // SAFETY: the buffer is valid for its length; the result is NUL-terminated on success.
    if unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } != 0 {
        return String::new();
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).into_owned()
}

#[cfg(not(unix))]
fn hostname() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::MaybeUninit;

    fn get(f: unsafe extern "C" fn(*mut VeltStr)) -> String {
        let mut out = MaybeUninit::uninit();
        // SAFETY: valid out-pointer; the (static or owned) string is copied then leaked in tests.
        unsafe {
            f(out.as_mut_ptr());
            String::from_utf8_lossy(out.assume_init().as_bytes()).into_owned()
        }
    }

    #[test]
    fn facts_are_plausible() {
        assert!(["darwin", "linux", "win32"].contains(&get(velt_rt_os_platform).as_str()));
        assert!(["x64", "arm64"].contains(&get(velt_rt_os_arch).as_str()));
        assert!(velt_rt_os_cpu_count() >= 1);
        assert!(!get(velt_rt_os_tmpdir).is_empty());
        let _ = get(velt_rt_os_hostname);
    }
}
