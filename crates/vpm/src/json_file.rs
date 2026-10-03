//! The package manager's generated files (`velt.lock.json`, the registry's `index.json`, a native
//! bundle's `native.json`) are JSON: pretty-printed, fields in declaration order and maps sorted
//! (stable diffs), with a trailing newline. Their former TOML names get an error that says what
//! replaced them.

use std::path::Path;

use serde::de::DeserializeOwned;
use serde::Serialize;

/// `value` as the text of a generated file.
pub fn to_text(value: &impl Serialize) -> String {
    let mut text = serde_json::to_string_pretty(value).expect("ICE: generated data serializes");
    text.push('\n');
    text
}

/// Parse generated-file text; the error names `what` (a file or its origin).
pub fn parse<T: DeserializeOwned>(text: &str, what: &str) -> Result<T, String> {
    serde_json::from_str(text).map_err(|e| format!("invalid {what}: {e}"))
}

/// Write `value` to `path` as a generated file, atomically ([`write_atomic`]).
pub fn write(path: &Path, value: &impl Serialize) -> Result<(), String> {
    write_atomic(path, &to_text(value))
}

/// Replace `path` with `text` atomically: a temporary file in the same directory, synced to disk
/// and renamed over it, so a concurrent reader (a registry server answering while a package is
/// published, a build reading the lock file) sees the old file or the new one, never a truncated
/// one, and a crash never leaves an empty one.
pub fn write_atomic(path: &Path, text: &str) -> Result<(), String> {
    write_atomic_as(path, text, false)
}

/// [`write`] for a file only its owner may read (mode 0600 on Unix from the moment it exists:
/// the temporary file is created that way, so the text is never readable by others).
pub fn write_private(path: &Path, value: &impl Serialize) -> Result<(), String> {
    write_atomic_as(path, &to_text(value), true)
}

fn create(path: &Path, private: bool) -> std::io::Result<std::fs::File> {
    #[cfg(windows)]
    if private {
        return create_owner_only(path);
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    #[cfg(not(any(unix, windows)))]
    let _ = private;
    options.open(path)
}

/// Windows: a new file whose DACL is protected (nothing inherited from the directory) and grants
/// access to its owner only, so a `$VELT_HOME` other users can read doesn't expose it.
#[cfg(windows)]
fn create_owner_only(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::FromRawHandle;
    use windows_sys::Win32::Foundation::{LocalFree, GENERIC_WRITE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
    use windows_sys::Win32::Storage::FileSystem::{CreateFileW, CREATE_NEW, FILE_ATTRIBUTE_NORMAL};
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    // Protected DACL, full access for the owner (OW: OWNER RIGHTS) and no one else.
    let sddl: Vec<u16> = "D:P(A;;FA;;;OW)".encode_utf16().chain([0]).collect();
    let mut descriptor = std::ptr::null_mut();
    // SAFETY: NUL-terminated wide strings that outlive the calls; the descriptor is freed with
    // LocalFree as the API requires, and the handle is owned by the returned File.
    unsafe {
        let ok = ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            std::ptr::null_mut(),
        );
        if ok == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        };
        let handle = CreateFileW(
            wide.as_ptr(),
            GENERIC_WRITE,
            0,
            &attributes,
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(),
        );
        let error = std::io::Error::last_os_error();
        LocalFree(descriptor);
        if handle == INVALID_HANDLE_VALUE {
            return Err(error);
        }
        Ok(std::fs::File::from_raw_handle(handle))
    }
}

fn write_atomic_as(path: &Path, text: &str, private: bool) -> Result<(), String> {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let name = path
        .file_name()
        .map_or_else(Default::default, |n| n.to_string_lossy());
    let tmp = path.with_file_name(format!(
        ".{name}.{}-{}.tmp",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let written = create(&tmp, private)
        .and_then(|mut f| f.write_all(text.as_bytes()).and_then(|()| f.sync_all()))
        .and_then(|()| std::fs::rename(&tmp, path));
    written.map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("cannot write `{}`: {e}", path.display())
    })?;
    sync_dir(path);
    Ok(())
}

/// Make a rename in `path`'s directory durable (Unix; Windows has no directory handle to sync).
/// Best effort: the rename itself has already succeeded.
fn sync_dir(path: &Path) {
    #[cfg(unix)]
    if let Some(dir) = path.parent() {
        let _ = std::fs::File::open(dir).and_then(|d| d.sync_all());
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// The error for a directory that still has the former TOML file `old` where `new` belongs.
pub fn legacy_error(old: &Path, new: &str, fix: &str) -> String {
    format!(
        "`{}` is no longer read (the file is now `{new}`): {fix}",
        old.display()
    )
}

#[cfg(test)]
mod tests {
    /// The SDDL of `path`'s DACL.
    #[cfg(windows)]
    fn dacl(path: &Path) -> String {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Foundation::LocalFree;
        use windows_sys::Win32::Security::Authorization::{
            ConvertSecurityDescriptorToStringSecurityDescriptorW, GetNamedSecurityInfoW,
            SDDL_REVISION_1, SE_FILE_OBJECT,
        };
        use windows_sys::Win32::Security::DACL_SECURITY_INFORMATION;
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
        let null = std::ptr::null_mut();
        let mut descriptor: windows_sys::Win32::Security::PSECURITY_DESCRIPTOR = null;
        let mut text = std::ptr::null_mut();
        let mut len = 0u32;
        // SAFETY: test-only reads of a file's security descriptor, freed with LocalFree.
        unsafe {
            let r = GetNamedSecurityInfoW(
                wide.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut descriptor,
            );
            assert_eq!(r, 0);
            let ok = ConvertSecurityDescriptorToStringSecurityDescriptorW(
                descriptor,
                SDDL_REVISION_1,
                DACL_SECURITY_INFORMATION,
                &mut text,
                &mut len,
            );
            assert_ne!(ok, 0);
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(text, len as usize));
            LocalFree(text.cast());
            LocalFree(descriptor);
            s.trim_end_matches('\0').to_string()
        }
    }

    #[cfg(windows)]
    #[test]
    fn private_files_are_owner_only_on_windows() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("credentials.json");
        write_private(&path, &BTreeMap::from([("a", 1)])).unwrap();
        write_private(&path, &BTreeMap::from([("b", 2)])).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "{
  \"b\": 2
}
"
        );
        let sddl = dacl(&path);
        assert!(sddl.starts_with("D:P"), "{sddl}");
        assert_eq!(sddl.matches("(A;").count(), 1, "{sddl}");
        assert!(sddl.contains(";;;OW)"), "{sddl}");
    }

    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn writes_replace_the_file_whole() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("index.json");
        write(&path, &BTreeMap::from([("a", 1)])).unwrap();
        write(&path, &BTreeMap::from([("b", 2)])).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "{\n  \"b\": 2\n}\n"
        );
        // No temporary file is left behind.
        assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 1);
        let e = write(&tmp.path().join("missing/x.json"), &1).unwrap_err();
        assert!(e.contains("cannot write"), "{e}");
    }

    #[test]
    fn pretty_sorted_and_newline_terminated() {
        let map = BTreeMap::from([("b", 2), ("a", 1)]);
        let text = to_text(&map);
        assert_eq!(text, "{\n  \"a\": 1,\n  \"b\": 2\n}\n");
        let back: BTreeMap<String, i32> = parse(&text, "test file").unwrap();
        assert_eq!(back["a"], 1);
        let err = parse::<BTreeMap<String, i32>>("a = 1", "`x.json`").unwrap_err();
        assert!(err.starts_with("invalid `x.json`: "), "{err}");
    }
}
