use super::*;

/// A prefix with `bin/velt` (executable) and a small `std/` tree.
fn prefix(at: &Path) {
    std::fs::create_dir_all(at.join("bin")).unwrap();
    std::fs::create_dir_all(at.join("std/prelude")).unwrap();
    std::fs::write(at.join("bin/velt"), b"#!").unwrap();
    std::fs::write(at.join("std/prelude/index.velt"), b"// prelude").unwrap();
    std::fs::write(at.join("std/fs.velt"), b"// fs").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let exe = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(at.join("bin/velt"), exe).unwrap();
    }
}

fn read_only(p: &Path) -> bool {
    std::fs::metadata(p).unwrap().permissions().readonly()
}

/// Whether this process is held to permissions (a root user on Unix is not: then a read-only
/// directory takes new files and loses old ones all the same).
fn enforced(tmp: &Path) -> bool {
    let dir = tmp.join("probe");
    std::fs::create_dir_all(dir.join("in")).unwrap();
    std::fs::write(dir.join("in/f"), b"").unwrap();
    set_tree(&dir.join("in"), true).unwrap();
    let held = std::fs::remove_file(dir.join("in/f")).is_err();
    remove_tree(&dir).unwrap();
    held
}

#[test]
fn std_goes_read_only_and_back() {
    let tmp = tempfile::tempdir().unwrap();
    let at = tmp.path().join("0.4.0");
    prefix(&at);
    set_std(&at, true).unwrap();
    assert!(read_only(&at.join("std/fs.velt")));
    assert!(read_only(&at.join("std/prelude/index.velt")));
    // Only std: the prefix's other files stay as they were.
    assert!(!read_only(&at.join("bin/velt")));
    #[cfg(unix)]
    {
        assert!(read_only(&at.join("std")), "the directory too, on Unix");
        if enforced(tmp.path()) {
            assert!(std::fs::write(at.join("std/new.velt"), b"").is_err());
        }
    }
    set_std(&at, false).unwrap();
    assert!(!read_only(&at.join("std/fs.velt")));
    assert!(!read_only(&at.join("std")));
    std::fs::write(at.join("std/fs.velt"), b"// edited").unwrap();
}

#[test]
fn a_read_only_toolchain_is_removed_whole() {
    let tmp = tempfile::tempdir().unwrap();
    let at = tmp.path().join("0.4.0");
    prefix(&at);
    set_std(&at, true).unwrap();
    // Without clearing the bit first, removing fails (Unix: a read-only directory keeps its
    // entries; Windows: a read-only file stays), which is why every removal goes through
    // `remove_tree`.
    if enforced(tmp.path()) {
        assert!(std::fs::remove_dir_all(&at).is_err());
        assert!(at.join("std/fs.velt").exists());
    }
    remove_tree(&at).unwrap();
    assert!(!at.exists());
}

#[test]
fn removing_a_missing_tree_says_so() {
    let tmp = tempfile::tempdir().unwrap();
    let err = remove_tree(&tmp.path().join("gone")).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::NotFound);
}
