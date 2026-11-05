//! The stamp stands for the tree only while nothing in it changed.

use super::*;

struct Tree {
    _tmp: tempfile::TempDir,
    loc: Locations,
    dir: PathBuf,
}

fn tree() -> Tree {
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
    checksum(&t.loc, &t.dir, expected, |e, a| {
        format!("mismatch: {e} vs {a}")
    })
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
