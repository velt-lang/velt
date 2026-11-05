//! Writing: staging and rename, nothing left behind on refusal, one checksum on every OS.

use super::*;
use crate::git::tree_checksum;

fn sample_tar() -> Vec<u8> {
    TarBuilder::default()
        .file("pkg/package.vlt", b"export const pkg = {}")
        .file("pkg/src/main.vlt", b"console.log(1)")
        .symlink("pkg/src/alias.vlt", "main.vlt")
        .hardlink("pkg/copy.vlt", "pkg/src/main.vlt")
        .dir("pkg/empty/")
        .gz()
}

#[test]
fn the_tree_is_written_and_renamed_into_place() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("cache/abc");
    extract(&sample_tar(), &dest).unwrap();
    assert_eq!(
        std::fs::read_to_string(dest.join("pkg/src/main.vlt")).unwrap(),
        "console.log(1)"
    );
    assert_eq!(
        std::fs::read_to_string(dest.join("pkg/copy.vlt")).unwrap(),
        "console.log(1)"
    );
    assert!(dest.join("pkg/empty").is_dir());
    let link = dest.join("pkg/src/alias.vlt");
    if cfg!(windows) {
        assert_eq!(std::fs::read_to_string(&link).unwrap(), "main.vlt");
    } else {
        assert_eq!(std::fs::read_link(&link).unwrap(), Path::new("main.vlt"));
    }
    // Only the destination is left in its parent: the staging directory was renamed.
    let left: Vec<_> = std::fs::read_dir(dir.path().join("cache"))
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(left, ["abc"]);
}

#[test]
fn a_refused_archive_leaves_nothing_behind() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("out");
    // Refused while reading: nothing is created, not even the staging directory.
    let bad = TarBuilder::default()
        .file("ok", b"")
        .file("../evil", b"")
        .gz();
    assert!(extract(&bad, &dest).is_err());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    // A destination that is already there is never replaced.
    std::fs::create_dir(&dest).unwrap();
    let e = extract(&sample_tar(), &dest).unwrap_err();
    assert!(e.contains("already exists"), "{e}");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn one_archive_has_one_checksum_on_every_os() {
    // Links as links (Unix) and as files holding their target (Windows, as git's
    // `core.symlinks=false`) hash the same, so the lock's checksum holds everywhere.
    let dir = tempfile::tempdir().unwrap();
    let as_links = dir.path().join("links");
    let as_files = dir.path().join("files");
    extract_within(&sample_tar(), &as_links, Limits::DEFAULT, Links::Symbolic).unwrap();
    extract_within(&sample_tar(), &as_files, Limits::DEFAULT, Links::Files).unwrap();
    let sum = tree_checksum(&as_links).unwrap();
    assert_eq!(sum, tree_checksum(&as_files).unwrap());
    // The same tree from a zip hashes the same too.
    let mut stored = ZipEntry::file("pkg/package.vlt", b"export const pkg = {}");
    stored.method = 0;
    let zip = zip(
        &[
            stored,
            ZipEntry::file("pkg/src/main.vlt", b"console.log(1)"),
            ZipEntry::symlink("pkg/src/alias.vlt", "main.vlt"),
            ZipEntry::file("pkg/copy.vlt", b"console.log(1)"),
        ],
        true,
    );
    let from_zip = dir.path().join("zip");
    extract(&zip, &from_zip).unwrap();
    assert_eq!(sum, tree_checksum(&from_zip).unwrap());
    // And the checksum sees the link: another target is another checksum.
    let other = TarBuilder::default()
        .file("pkg/package.vlt", b"export const pkg = {}")
        .file("pkg/src/main.vlt", b"console.log(1)")
        .symlink("pkg/src/alias.vlt", "./main.vlt")
        .hardlink("pkg/copy.vlt", "pkg/src/main.vlt")
        .gz();
    let changed = dir.path().join("changed");
    extract(&other, &changed).unwrap();
    assert_ne!(sum, tree_checksum(&changed).unwrap());
}

#[test]
fn extractions_into_one_parent_get_their_own_staging_directories() {
    let dir = tempfile::tempdir().unwrap();
    let tar = TarBuilder::default().file("a", b"x").gz();
    for i in 0..50 {
        extract(&tar, &dir.path().join(format!("{i}"))).unwrap();
    }
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 50);
}
