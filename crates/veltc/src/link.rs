//! The link step of `velt build`: program objects → executable. Picks how the runtime is linked
//! (debug builds: the shared runtime when it is installed, so a link takes milliseconds; release
//! builds and `$VELT_RT_LIB`: the static one) and skips the link entirely when its inputs are
//! unchanged since the last link of the same executable (a stamp file beside it), so `velt run`
//! twice in a row links once.

use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// What the link step did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Linked {
    /// The linker ran.
    Linked,
    /// The executable was already linked from the same inputs.
    UpToDate,
}

/// Link the program's objects (the first is the main one) into `exe` for `target`.
pub fn link_executable(
    target: &str,
    program_objects: &[PathBuf],
    exe: &Path,
    release: bool,
    strip_debug: bool,
    native: &[velt_link::NativeLink],
) -> Result<Linked, String> {
    let shared = if release {
        None
    } else {
        velt_link::find_shared_runtime_lib(target)
    };
    let runtime_lib = match shared {
        Some(lib) => lib,
        None => velt_link::find_runtime_lib(target)?,
    };
    let object = program_objects
        .first()
        .ok_or("ICE: linking a program without objects")?;
    let mut objects = program_objects.to_vec();
    if velt_link::is_shared_runtime_lib(&runtime_lib, target) {
        let bytes = velt_codegen_cl::emit_entry_object(target)?;
        let entry = entry_object_path(object);
        write_if_changed(&entry, &bytes)?;
        objects.push(entry);
    }
    let stamp_path = stamp_path(exe);
    let linker = velt_link::linker_identity(target);
    let key = link_key(target, &objects, &runtime_lib, strip_debug, native, &linker);
    if std::fs::read_to_string(&stamp_path).ok() == Some(stamp(&key, exe)) {
        return Ok(Linked::UpToDate);
    }
    // A link that fails half-way must not leave a stamp that matches the old executable.
    let _ = std::fs::remove_file(&stamp_path);
    velt_link::link(&velt_link::LinkRequest {
        target,
        objects: &objects,
        runtime_lib: &runtime_lib,
        output: exe,
        release: strip_debug,
        native,
    })?;
    // Best effort: without a stamp the next build just links again.
    let _ = std::fs::write(&stamp_path, stamp(&key, exe));
    Ok(Linked::Linked)
}

/// `<dir>/<name>.entry.<ext>` beside the program object `<dir>/<name>.<ext>`.
fn entry_object_path(object: &Path) -> PathBuf {
    let stem = object.file_stem().unwrap_or_default().to_string_lossy();
    let ext = object.extension().unwrap_or_default().to_string_lossy();
    object.with_file_name(format!("{stem}.entry.{ext}"))
}

/// `<exe>.link-stamp`.
fn stamp_path(exe: &Path) -> PathBuf {
    let mut name = exe.file_name().unwrap_or_default().to_os_string();
    name.push(".link-stamp");
    exe.with_file_name(name)
}

fn write_if_changed(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if std::fs::read(path).is_ok_and(|old| old == bytes) {
        return Ok(());
    }
    std::fs::write(path, bytes).map_err(|e| format!("cannot write `{}`: {e}", path.display()))
}

/// Hash of everything the link reads or is configured by: target, settings, object contents, the
/// runtime library and native libraries (path, size, modification time) and the linker
/// ([`velt_link::linker_identity`]: the bundled lld and kit, or `$VELT_LINKER`).
fn link_key(
    target: &str,
    objects: &[PathBuf],
    runtime_lib: &Path,
    strip_debug: bool,
    native: &[velt_link::NativeLink],
    linker: &str,
) -> String {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (target, strip_debug).hash(&mut h);
    for o in objects {
        std::fs::read(o).ok().hash(&mut h);
    }
    runtime_lib.hash(&mut h);
    file_stamp(runtime_lib).hash(&mut h);
    for n in native {
        for file in [
            Some(&n.shared),
            n.import_lib.as_ref(),
            n.static_obj.as_ref(),
        ] {
            file.hash(&mut h);
            file.and_then(|f| file_stamp(f)).hash(&mut h);
        }
    }
    linker.hash(&mut h);
    format!("{:016x}", h.finish())
}

/// The stamp file's contents: the link key and the executable it produced (size, modification
/// time), so a replaced or deleted executable is linked again.
fn stamp(key: &str, exe: &Path) -> String {
    format!("{key} {:?}\n", file_stamp(exe))
}

fn file_stamp(path: &Path) -> Option<(u64, SystemTime)> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.len(), meta.modified().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn side_files() {
        let dir = Path::new("target").join("velt");
        assert_eq!(
            entry_object_path(&dir.join("app.o")),
            dir.join("app.entry.o")
        );
        assert_eq!(
            stamp_path(&dir.join("app.exe")),
            dir.join("app.exe.link-stamp")
        );
    }

    #[test]
    fn key_changes_with_the_inputs() {
        let tmp = tempfile::tempdir().unwrap();
        let (obj, rt) = (tmp.path().join("a.o"), tmp.path().join("rt.a"));
        std::fs::write(&obj, b"one").unwrap();
        std::fs::write(&rt, b"runtime").unwrap();
        let objs = [obj.clone()];
        let key = link_key("t", &objs, &rt, false, &[], "L");
        assert_eq!(key, link_key("t", &objs, &rt, false, &[], "L"));
        assert_ne!(key, link_key("t", &objs, &rt, true, &[], "L"));
        assert_ne!(key, link_key("u", &objs, &rt, false, &[], "L"));
        let native = [velt_link::NativeLink {
            shared: rt.clone(),
            import_lib: None,
            static_obj: None,
        }];
        assert_ne!(key, link_key("t", &objs, &rt, false, &native, "L"));
        // Another linker (bundled ↔ system, another kit) links again.
        assert_ne!(key, link_key("t", &objs, &rt, false, &[], "M"));
        std::fs::write(&obj, b"two").unwrap();
        assert_ne!(key, link_key("t", &objs, &rt, false, &[], "L"));
    }

    #[test]
    fn link_and_codegen_agree_on_the_host() {
        // The runtime lookup tells the host target from others with velt_link's copy.
        assert_eq!(velt_link::host_triple(), velt_codegen_cl::host_triple());
    }

    #[test]
    fn stamp_tracks_the_executable() {
        let tmp = tempfile::tempdir().unwrap();
        let exe = tmp.path().join("app");
        let missing = stamp("k", &exe);
        std::fs::write(&exe, b"exe").unwrap();
        assert_ne!(stamp("k", &exe), missing);
        assert_eq!(stamp("k", &exe), stamp("k", &exe));
    }
}
