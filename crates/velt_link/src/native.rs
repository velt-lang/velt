//! Native libraries of packages in a link (docs/internals/contracts/native_abi.md "Linking"): a
//! prelinked object goes with the program's objects; a shared library is linked by `-l` name
//! with an rpath on Unix, or by its import library on Windows, where the DLL is then copied
//! beside the executable.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// One package's native library in a link.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeLink {
    /// The shared library (`.so`, `.dylib`, `.dll`).
    pub shared: PathBuf,
    /// Windows: the import library of `shared`.
    pub import_lib: Option<PathBuf>,
    /// Link this prelinked object statically instead of `shared` (Linux and macOS release
    /// builds; `None` links `shared`).
    pub static_obj: Option<PathBuf>,
}

/// Every file the link will use exists.
pub(crate) fn check_files(native: &[NativeLink]) -> Result<(), String> {
    for n in native {
        let file = n.static_obj.as_ref().unwrap_or(&n.shared);
        if !file.is_file() {
            return Err(format!("native library not found: {}", file.display()));
        }
    }
    Ok(())
}

/// `link.exe` arguments: each library's import library.
pub(crate) fn msvc_args(native: &[NativeLink]) -> Result<Vec<OsString>, String> {
    native
        .iter()
        .map(|n| {
            n.import_lib.as_ref().map(OsString::from).ok_or_else(|| {
                format!(
                    "native library {} has no import library",
                    n.shared.display()
                )
            })
        })
        .collect()
}

/// Windows: copy each DLL beside the executable.
pub(crate) fn place_dlls(native: &[NativeLink], output: &Path) -> Result<(), String> {
    for n in native {
        crate::shared::place_file(&n.shared, output)?;
    }
    Ok(())
}

/// The prelinked objects (linked like the program's own objects).
pub(crate) fn static_objects(native: &[NativeLink]) -> impl Iterator<Item = OsString> + '_ {
    native
        .iter()
        .filter_map(|n| n.static_obj.as_ref())
        .map(OsString::from)
}

/// `cc` arguments for the libraries linked as shared libraries.
pub(crate) fn unix_shared_args(native: &[NativeLink]) -> Vec<OsString> {
    native
        .iter()
        .filter(|n| n.static_obj.is_none())
        .flat_map(|n| crate::shared::unix_lib_args(&n.shared))
        .collect()
}

/// The bundled lld's arguments for the libraries linked as shared libraries.
pub(crate) fn lld_shared_args(native: &[NativeLink]) -> Vec<OsString> {
    native
        .iter()
        .filter(|n| n.static_obj.is_none())
        .flat_map(|n| crate::shared::lld_shared_lib_args(&n.shared))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{msvc_args as link_msvc_args, unix_args, LinkRequest, TargetOs};

    #[test]
    fn native_libraries_in_unix_and_msvc_args() {
        let objs = [PathBuf::from("p.o")];
        let native = [
            NativeLink {
                shared: PathBuf::from("/c/a/libvelt_native_a.so"),
                import_lib: None,
                static_obj: Some(PathBuf::from("/c/a/a.o")),
            },
            NativeLink {
                shared: PathBuf::from("/c/b/libvelt_native_b.so"),
                import_lib: None,
                static_obj: None,
            },
        ];
        let req = LinkRequest {
            target: "x86_64-unknown-linux-gnu",
            objects: &objs,
            runtime_lib: Path::new("libvelt_rt.a"),
            output: Path::new("out"),
            release: true,
            native: &native,
        };
        let a: Vec<String> = unix_args(&req, TargetOs::Linux)
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        let pos = |s: &str| a.iter().position(|x| x == s).unwrap();
        // The prelinked object goes with the program's objects; the shared one by `-l` name.
        assert!(pos("p.o") < pos("/c/a/a.o") && pos("/c/a/a.o") < pos("libvelt_rt.a"));
        assert!(pos("-lvelt_native_b") > pos("libvelt_rt.a"));
        assert!(!a.iter().any(|x| x.contains("velt_native_a.so")));

        let dll = [NativeLink {
            shared: PathBuf::from("C:/c/b/b.dll"),
            import_lib: Some(PathBuf::from("C:/c/b/b.dll.lib")),
            static_obj: None,
        }];
        let req = LinkRequest {
            target: "x86_64-pc-windows-msvc",
            runtime_lib: Path::new("velt_rt.lib"),
            native: &dll,
            ..req
        };
        let a: Vec<String> = link_msvc_args(&req)
            .unwrap()
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert!(
            a.iter().position(|x| x == "C:/c/b/b.dll.lib")
                < a.iter().position(|x| x == "velt_rt.lib")
        );
    }
}
