//! The stamp stands for the tree only while nothing in it changed.

use super::*;

thread_local! {
    /// Stamp trees made just now, which [`is_racy`] otherwise leaves unstamped: these tests make
    /// a tree and stamp it at once, with no sleep.
    pub(super) static NO_RACY_WINDOW: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// How far ahead of the system clock [`is_racy`] takes now to be, to see a tree as it is
    /// later without setting its times back.
    pub(super) static CLOCK_AHEAD: std::cell::Cell<std::time::Duration> =
        const { std::cell::Cell::new(std::time::Duration::ZERO) };
}

struct Tree {
    _tmp: tempfile::TempDir,
    loc: Locations,
    dir: PathBuf,
}

fn tree() -> Tree {
    NO_RACY_WINDOW.with(|c| c.set(true));
    let tmp = tempfile::tempdir().unwrap();
    let loc = Locations::under(&tmp.path().join("home"));
    let dir = tmp.path().join("tree");
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("package.velt"), "export const pkg = {};\n").unwrap();
    std::fs::write(dir.join("src/lib.velt"), "export const v = 1;\n").unwrap();
    Tree {
        _tmp: tmp,
        loc,
        dir,
    }
}

fn sum(t: &Tree, expected: Option<&str>) -> Result<String, String> {
    sum_by(t, expected, TreeCheck::Files)
}

fn sum_by(t: &Tree, expected: Option<&str>, depth: TreeCheck) -> Result<String, String> {
    let mismatch = |e: &str, a: &str| format!("mismatch: {e} vs {a}");
    checksum(&t.loc, &t.dir, (expected, depth), mismatch, || Ok(()))
}

#[test]
fn an_unchanged_tree_is_taken_from_its_stamp() {
    let t = tree();
    let real = sum(&t, None).unwrap();
    assert_eq!(real, crate::git::tree_checksum(&t.dir).unwrap());
    // Proof the stamp is read: a stamp naming another checksum for this very tree is believed.
    let stamp = stamp_path(&t.loc, &t.dir);
    let text = std::fs::read_to_string(&stamp).unwrap();
    let forged = text.replacen(&real, "sha256:stamped", 1);
    std::fs::write(&stamp, forged).unwrap();
    assert_eq!(sum(&t, None).unwrap(), "sha256:stamped");
    // Without a stamp, the tree is read.
    std::fs::remove_file(&stamp).unwrap();
    assert_eq!(sum(&t, Some(&real)).unwrap(), real);
}

#[test]
fn an_edited_tree_is_hashed_again() {
    let t = tree();
    let real = sum(&t, None).unwrap();
    // Same size, another byte.
    std::fs::write(t.dir.join("src/lib.velt"), "export const v = 2;\n").unwrap();
    let e = sum(&t, Some(&real)).unwrap_err();
    assert!(e.starts_with("mismatch"), "{e}");
    // A file added, then one removed.
    let t = tree();
    let real = sum(&t, None).unwrap();
    std::fs::write(t.dir.join("src/new.velt"), "").unwrap();
    assert!(sum(&t, Some(&real)).is_err());
    std::fs::remove_file(t.dir.join("src/new.velt")).unwrap();
    std::fs::remove_file(t.dir.join("src/lib.velt")).unwrap();
    assert!(sum(&t, Some(&real)).is_err());
}

/// On Unix, an edit that restores the size and the modification time still changes the inode
/// change time, which its owner can't set back.
#[cfg(unix)]
#[test]
fn an_edit_that_restores_the_modification_time_is_still_caught() {
    let t = tree();
    let real = sum(&t, None).unwrap();
    let file = t.dir.join("src/lib.velt");
    let modified = std::fs::metadata(&file).unwrap().modified().unwrap();
    std::fs::write(&file, "export const v = 9;\n").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(modified)
        .unwrap();
    assert_eq!(
        std::fs::metadata(&file).unwrap().modified().unwrap(),
        modified
    );
    let e = sum(&t, Some(&real)).unwrap_err();
    assert!(e.starts_with("mismatch"), "{e}");
}

#[test]
fn a_checksum_the_lock_disagrees_with_is_not_stamped() {
    let t = tree();
    assert!(sum(&t, Some("sha256:other")).is_err());
    assert!(!stamp_path(&t.loc, &t.dir).exists());
}

/// A build looks only at the directories: a file added, removed or renamed is caught; an edit in
/// place is left to the next check of every file (`velt install`, `update`, `add`).
#[test]
fn a_build_looks_only_at_directories() {
    let t = tree();
    let real = sum(&t, None).unwrap();
    // Proof only the directories are looked at: a forged stamp is believed while they are
    // unchanged, though a file's metadata changed.
    let stamp = stamp_path(&t.loc, &t.dir);
    let text = std::fs::read_to_string(&stamp).unwrap();
    std::fs::write(&stamp, text.replacen(&real, "sha256:stamped", 1)).unwrap();
    // A file's own times change (as an edit in place can leave its directory's alone).
    let file = t.dir.join("src/lib.velt");
    let earlier = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    std::fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(earlier)
        .unwrap();
    assert_eq!(
        sum_by(&t, None, TreeCheck::Directories).unwrap(),
        "sha256:stamped"
    );
    // Every file's check sees it, reads the tree, and finds it unchanged.
    assert_eq!(sum_by(&t, Some(&real), TreeCheck::Files).unwrap(), real);
    // A file added, removed or renamed changes a directory's times.
    for change in ["add", "remove", "rename"] {
        let t = tree();
        let real = sum(&t, None).unwrap();
        match change {
            "add" => std::fs::write(t.dir.join("src/new.velt"), "").unwrap(),
            "remove" => std::fs::remove_file(t.dir.join("src/lib.velt")).unwrap(),
            _ => std::fs::rename(t.dir.join("src/lib.velt"), t.dir.join("src/moved.velt")).unwrap(),
        }
        assert!(
            sum_by(&t, Some(&real), TreeCheck::Directories).is_err(),
            "{change}"
        );
    }
}

/// A tree changed within the times' resolution of now isn't stamped: an edit in that window
/// could leave every time as stamped.
#[test]
fn a_tree_changed_just_now_is_not_stamped() {
    let t = tree();
    NO_RACY_WINDOW.with(|c| c.set(false));
    sum(&t, None).unwrap();
    assert!(!stamp_path(&t.loc, &t.dir).exists());
    // The same tree an hour later is. The clock moves rather than the tree's times: inode change
    // times can't be set back, and on Windows the directories' times would still be now.
    CLOCK_AHEAD.with(|c| c.set(std::time::Duration::from_secs(3600)));
    sum(&t, None).unwrap();
    CLOCK_AHEAD.with(|c| c.set(std::time::Duration::ZERO));
    assert!(stamp_path(&t.loc, &t.dir).exists());
}

/// `velt install --locked` reads every byte: a forged stamp is not believed.
#[test]
fn every_byte_ignores_the_stamp() {
    let t = tree();
    let real = sum(&t, None).unwrap();
    std::fs::write(t.dir.join("src/lib.velt"), "export const v = 7;\n").unwrap();
    let edited = crate::git::tree_checksum(&t.dir).unwrap();
    // A stamp written for the edited tree, naming the lock's checksum.
    sum(&t, None).unwrap();
    let stamp = stamp_path(&t.loc, &t.dir);
    let text = std::fs::read_to_string(&stamp).unwrap();
    std::fs::write(&stamp, text.replacen(&edited, &real, 1)).unwrap();
    assert_eq!(sum_by(&t, Some(&real), TreeCheck::Files).unwrap(), real);
    let e = sum_by(&t, Some(&real), TreeCheck::Bytes).unwrap_err();
    assert!(e.starts_with("mismatch"), "{e}");
}

/// The directory check still compares the stamped checksum with the lock's.
#[test]
fn the_directory_check_compares_with_the_lock() {
    let t = tree();
    sum(&t, None).unwrap();
    let e = sum_by(&t, Some("sha256:other"), TreeCheck::Directories).unwrap_err();
    assert!(e.starts_with("mismatch"), "{e}");
}

/// `velt install --locked` reads every byte; a build keeps its directory check, `--locked` or not.
#[test]
fn an_install_with_locked_reads_every_byte() {
    use crate::install::InstallOptions;
    let opts = |locked, tree_check| InstallOptions {
        locked,
        tree_check,
        ..Default::default()
    };
    assert_eq!(for_install(&opts(true, TreeCheck::Files)), TreeCheck::Bytes);
    assert_eq!(
        for_install(&opts(false, TreeCheck::Files)),
        TreeCheck::Files
    );
    let build = TreeCheck::Directories;
    assert_eq!(for_install(&opts(true, build)), build);
}
