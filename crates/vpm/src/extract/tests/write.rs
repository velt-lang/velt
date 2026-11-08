//! Writing: staging and rename, nothing left behind on refusal, one checksum on every OS.

use super::*;
use crate::git::tree_checksum;

fn sample_tar() -> Vec<u8> {
    TarBuilder::default()
        .file("pkg/package.velt", b"export const pkg = {}")
        .file("pkg/src/main.velt", b"console.log(1)")
        .symlink("pkg/src/alias.velt", "main.velt")
        .hardlink("pkg/copy.velt", "pkg/src/main.velt")
        .dir("pkg/empty/")
        .gz()
}

#[test]
fn the_tree_is_written_and_renamed_into_place() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("cache/abc");
    extract(&sample_tar(), &dest).unwrap();
    assert_eq!(
        std::fs::read_to_string(dest.join("pkg/src/main.velt")).unwrap(),
        "console.log(1)"
    );
    assert_eq!(
        std::fs::read_to_string(dest.join("pkg/copy.velt")).unwrap(),
        "console.log(1)"
    );
    assert!(dest.join("pkg/empty").is_dir());
    let link = dest.join("pkg/src/alias.velt");
    if cfg!(windows) {
        assert_eq!(std::fs::read_to_string(&link).unwrap(), "main.velt");
    } else {
        assert_eq!(std::fs::read_link(&link).unwrap(), Path::new("main.velt"));
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
    let mut stored = ZipEntry::file("pkg/package.velt", b"export const pkg = {}");
    stored.method = 0;
    let zip = zip(
        &[
            stored,
            ZipEntry::file("pkg/src/main.velt", b"console.log(1)"),
            ZipEntry::symlink("pkg/src/alias.velt", "main.velt"),
            ZipEntry::file("pkg/copy.velt", b"console.log(1)"),
        ],
        true,
    );
    let from_zip = dir.path().join("zip");
    extract(&zip, &from_zip).unwrap();
    assert_eq!(sum, tree_checksum(&from_zip).unwrap());
    // And the checksum sees the link: another target is another checksum.
    let other = TarBuilder::default()
        .file("pkg/package.velt", b"export const pkg = {}")
        .file("pkg/src/main.velt", b"console.log(1)")
        .symlink("pkg/src/alias.velt", "./main.velt")
        .hardlink("pkg/copy.velt", "pkg/src/main.velt")
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

#[test]
fn what_is_ready_is_recorded_before_the_tree_is_seen_and_a_replaced_tree_stays_until_then() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("abc");
    let tree = read(&sample_tar(), Limits::DEFAULT).unwrap();
    write_then(&tree, (&dest, Place::New), Links::NATIVE, |staged| {
        assert!(staged.join("pkg/src/main.velt").is_file());
        assert!(!dest.exists(), "renamed before it was ready");
        Ok(())
    })
    .unwrap();
    // New refuses what is there; Replace keeps it until the new tree is ready, and keeps it
    // when that fails.
    assert!(write(&tree, &dest, Links::NATIVE).is_err());
    std::fs::write(dest.join("old"), "x").unwrap();
    let failed = write_then(&tree, (&dest, Place::Replace), Links::NATIVE, |_| {
        Err("no".to_string())
    });
    assert_eq!(failed, Err("no".to_string()));
    assert!(dest.join("old").is_file());
    write_then(&tree, (&dest, Place::Replace), Links::NATIVE, |_| {
        assert!(
            dest.join("old").is_file(),
            "removed before the new tree was ready"
        );
        Ok(())
    })
    .unwrap();
    assert!(!dest.join("old").exists());
    assert!(dest.join("pkg/src/main.velt").is_file());
    // Nothing is left beside it.
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn staging_left_by_a_killed_extraction_is_removed_once_it_is_old() {
    let dir = tempfile::tempdir().unwrap();
    let left = [
        ".abc.partial-1-0",
        ".abc.partial-1-0.old",
        ".def.partial-9-3",
    ];
    for name in left {
        std::fs::create_dir(dir.path().join(name)).unwrap();
        std::fs::write(dir.path().join(name).join("f"), "x").unwrap();
    }
    // Other names stay, whatever their age: a tree, its record, a lock.
    // `abc.partial-1` too: staging names start with a dot.
    let kept = [
        "abc",
        "abc.partial-1",
        "abc.verified",
        "abc.lock",
        ".hidden",
    ];
    for name in &kept[..2] {
        std::fs::create_dir(dir.path().join(name)).unwrap();
    }
    for name in &kept[2..] {
        std::fs::write(dir.path().join(name), "x").unwrap();
    }
    // Just made: a running extraction's, kept.
    let now = std::time::SystemTime::now();
    super::super::remove_stale_staging(dir.path(), now);
    assert!(left.iter().all(|n| dir.path().join(n).exists()));
    // A day later, as a suspended extraction could be: kept.
    let day = now + std::time::Duration::from_secs(24 * 3600);
    super::super::remove_stale_staging(dir.path(), day);
    assert!(left.iter().all(|n| dir.path().join(n).exists()));
    // Eight days later: removed.
    let later = now + std::time::Duration::from_secs(8 * 24 * 3600);
    super::super::remove_stale_staging(dir.path(), later);
    assert!(left.iter().all(|n| !dir.path().join(n).exists()));
    assert!(kept.iter().all(|n| dir.path().join(n).exists()));
}

// Opening a directory to set its time needs another flag on Windows; the sweep itself is tested
// above on every OS.
#[cfg(unix)]
#[test]
fn an_extraction_removes_staging_left_beside_it() {
    let dir = tempfile::tempdir().unwrap();
    let left = dir.path().join(".other.partial-1-0");
    std::fs::create_dir(&left).unwrap();
    let eight_days = std::time::Duration::from_secs(8 * 24 * 3600);
    let long_ago = std::time::SystemTime::now() - eight_days;
    (std::fs::File::open(&left).unwrap())
        .set_modified(long_ago)
        .unwrap();
    let tar = TarBuilder::default().file("f", b"x").gz();
    let tree = read(&tar, Limits::DEFAULT).unwrap();
    super::super::write(&tree, &dir.path().join("dest"), Links::NATIVE).unwrap();
    assert!(!left.exists());
}
