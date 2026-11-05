//! Archives built to make a check cost too much or to slip past one: folds the clash key could
//! miss, deep trees, long link walks, zips refused only after decompressing, and a disk that
//! already holds something in the tree's way.

use super::*;

const LOOSE: Limits = Limits {
    total: 4 << 20,
    entries: 100_000,
    ratio: 100_000,
    names: 64 << 20,
};

#[test]
fn sharp_s_in_every_case_is_one_name() {
    // `ẞ` lowercases to `ß`, which uppercases to `SS`: one round of folding keeps `ẞ` apart.
    for (a, b) in [("\u{df}", "\u{1e9e}"), ("\u{1e9e}", "ss"), ("\u{df}", "SS")] {
        let tar = TarBuilder::default().file(a, b"1").file(b, b"2").gz();
        refused(&tar, "differ only by case");
    }
    // The escape that split made: `qß` goes through the link `d/d/pß` as `d/d/pẞ`.
    let escape = TarBuilder::default()
        .symlink("d/d/p\u{df}", "../..")
        .symlink("q\u{df}", "d/d/p\u{1e9e}/../..")
        .gz();
    refused(&escape, "leads outside the archive");
    let under = TarBuilder::default()
        .symlink("q\u{df}", "x")
        .file("q\u{1e9e}/evil", b"")
        .gz();
    refused(&under, "differ only by case");
}

#[cfg(unix)]
#[test]
fn nothing_is_written_through_what_is_already_on_disk() {
    // A directory or file the filesystem takes for one already there (a fold the key doesn't
    // know) is an error, not a write through it: here, links already in the root.
    let dir = tempfile::tempdir().unwrap();
    let outside = dir.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("kept"), b"before").unwrap();
    for (tar, link_to) in [
        (
            TarBuilder::default().file("d/evil", b"x").gz(),
            outside.clone(),
        ),
        (
            TarBuilder::default().file("f", b"x").gz(),
            outside.join("kept"),
        ),
    ] {
        let root = dir
            .path()
            .join(format!("root{}", link_to.as_os_str().len()));
        std::fs::create_dir(&root).unwrap();
        let name = if link_to == outside { "d" } else { "f" };
        std::os::unix::fs::symlink(&link_to, root.join(name)).unwrap();
        let tree = read(&tar, Limits::DEFAULT).unwrap();
        let e = write_into(&tree, &root, Links::Symbolic).unwrap_err();
        assert!(e.contains("cannot write"), "{e}");
    }
    assert!(!outside.join("evil").exists());
    assert_eq!(std::fs::read(outside.join("kept")).unwrap(), b"before");
}

#[test]
fn entries_under_one_deep_directory_cost_linear_work() {
    // 300 files under one 128-deep directory: each walk of its parents again would be 128 claims
    // and keys per file.
    let deep = vec!["d"; 127].join("/");
    let mut tar = TarBuilder::default();
    for i in 0..300 {
        tar = tar
            .pax(b'x', &[("path", &format!("{deep}/f{i}"))])
            .file("x", b"");
    }
    let tree = read(&tar.gz(), LOOSE).unwrap();
    assert!(tree.work < 2 * (127 + 300) + 10, "{}", tree.work);
    // A directory listed again costs no key either.
    let mut again = TarBuilder::default();
    for _ in 0..300 {
        again = again.pax(b'x', &[("path", &deep)]).dir("x");
    }
    let tree = read(&again.gz(), LOOSE).unwrap();
    assert!(tree.work < 2 * 127 + 300 + 10, "{}", tree.work);
}

#[test]
fn the_bytes_of_all_names_are_capped() {
    // Deep paths with new prefixes: every directory above each is new.
    let tight = Limits {
        names: 1 << 20,
        ..LOOSE
    };
    let mut tar = TarBuilder::default();
    for i in 0..40 {
        let path = format!(
            "p{i}/{}/f",
            vec!["segment-of-thirty-bytes-long-x"; 120].join("/")
        );
        tar = tar.pax(b'x', &[("path", &path)]).file("x", b"");
    }
    let e = read(&tar.gz(), tight).unwrap_err();
    assert!(e.contains("its paths add up to more than 1024 KiB"), "{e}");
}

#[test]
fn a_link_walk_is_capped_by_the_bytes_it_looks_at() {
    // Few segments, long keys: `b` passes through `D/a` (a link to `.`) 200 times with a 3 KB key
    // each time and climbs back to the root, and `c` follows `b` 2000 times: about 1.3 GB looked
    // at in some 600,000 segments.
    let long = vec!["x".repeat(59); 50].join("/");
    let b_target = format!("{long}{}{}", "/a".repeat(200), "/..".repeat(50));
    let tar = TarBuilder::default()
        .pax(b'x', &[("path", &format!("{long}/a"))])
        .symlink("x", ".")
        .pax(b'x', &[("linkpath", &b_target)])
        .symlink("b", "x")
        .pax(b'x', &[("linkpath", &vec!["b"; 2000].join("/"))])
        .symlink("c", "x")
        .gz();
    refused(&tar, "takes too many steps to follow");
}

/// The local part (headers and data) and the central entries of a zip built by [`zip`].
fn split(z: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let at = |sig: u32| (z.windows(4)).position(|w| w == sig.to_le_bytes()).unwrap();
    (
        z[..at(0x0201_4b50)].to_vec(),
        z[at(0x0201_4b50)..at(0x0605_4b50)].to_vec(),
    )
}

/// A zip of `local`, then `central`, then an end record counting `n` entries.
fn assemble(local: &[u8], central: &[u8], n: u16) -> Vec<u8> {
    let mut out = local.to_vec();
    out.extend_from_slice(central);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&n.to_le_bytes());
    out.extend_from_slice(&n.to_le_bytes());
    out.extend_from_slice(&(central.len() as u32).to_le_bytes());
    out.extend_from_slice(&(local.len() as u32).to_le_bytes());
    out.extend_from_slice(&[0, 0]);
    out
}

#[test]
fn a_zip_is_refused_before_anything_is_decompressed() {
    // Overlapping entries whose shared data is damaged: the overlap is found first, so nothing
    // of it is decompressed (the bomb's cost).
    let (mut local_b, mut central_b) = split(&zip(&[ZipEntry::file("b", b"payload")], false));
    local_b[14] ^= 1;
    central_b[16] ^= 1;
    let mut a = ZipEntry::file("a", &local_b);
    a.method = 0;
    let (local_a, central_a) = split(&zip(&[a], false));
    central_b[42..46].copy_from_slice(&31u32.to_le_bytes());
    let both = [central_b, central_a].concat();
    refused(
        &assemble(&local_a, &both, 2),
        "two entries of the zip share data",
    );
    // Declared sizes past the limit, with damaged data: refused by the sizes.
    let big = vec![b'x'; 3 << 20];
    let mut z = zip(
        &[ZipEntry::file("a", &big), ZipEntry::file("b", &big)],
        false,
    );
    let second = (z.windows(4))
        .rposition(|w| w == 0x0403_4b50u32.to_le_bytes())
        .unwrap();
    z[second + 40] ^= 0xff;
    let e = read(&z, LOOSE).unwrap_err();
    assert!(e.contains("more than 4 MiB"), "{e}");
}

#[test]
fn every_byte_of_a_zip_belongs_to_an_entry() {
    let one = zip(&[ZipEntry::file("a", b"x")], false);
    let (local, central) = split(&one);
    // Bytes before the first entry that look like a local header, with offsets moved past them.
    let mut gap = b"PK\x03\x04hidden".to_vec();
    gap.extend_from_slice(&local);
    let mut moved = central.clone();
    moved[42..46].copy_from_slice(&10u32.to_le_bytes());
    refused(
        &assemble(&gap, &moved, 1),
        "bytes before or between its entries",
    );
    // Bytes between the last entry and the central directory.
    let mut tail = local.clone();
    tail.extend_from_slice(b"junk");
    refused(
        &assemble(&tail, &central, 1),
        "between its last entry and its central directory",
    );
    // Bytes after the last central entry.
    let mut extra = central.clone();
    extra.extend_from_slice(b"junk");
    refused(&assemble(&local, &extra, 1), "data after its last entry");
    // A zip64 record in a central entry's extra field.
    let mut z64 = central.clone();
    z64[30..32].copy_from_slice(&4u16.to_le_bytes());
    z64.extend_from_slice(&[1, 0, 0, 0]);
    refused(&assemble(&local, &z64, 1), "a zip64 entry");
}

#[test]
fn a_tar_entry_past_the_budget_is_refused_before_it_is_read() {
    // The second file's header claims 3 MiB that the archive doesn't hold: refused by the
    // budget, not by reading to the end.
    let mut tar = TarBuilder::default().file("a", &vec![b'x'; 3 << 20]).tar();
    tar.truncate(tar.len() - 1024);
    tar.extend_from_slice(&header(b'0', b"b", 3 << 20, ""));
    // Few entries, so the budget's allowance for headers is small.
    let few = Limits {
        entries: 10,
        ..LOOSE
    };
    let e = read(&gz(&tar), few).unwrap_err();
    assert!(e.contains("more than"), "{e}");
}

#[test]
fn a_control_character_in_a_link_target_is_refused() {
    let tar = TarBuilder::default().symlink("l", "a\u{1}b").gz();
    refused(&tar, "control character in a link target");
}

#[test]
fn a_failed_write_leaves_no_staging_behind() {
    // A name longer than any filesystem's 255 bytes passes the checks and fails when written.
    let dir = tempfile::tempdir().unwrap();
    let tar = TarBuilder::default()
        .pax(b'x', &[("path", &"n".repeat(300))])
        .file("x", b"")
        .gz();
    assert!(extract(&tar, &dir.path().join("out")).is_err());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn parallel_extractions_into_one_parent_each_get_their_own_staging() {
    let dir = tempfile::tempdir().unwrap();
    let tar = TarBuilder::default().file("a", b"x").gz();
    std::thread::scope(|s| {
        for t in 0..8 {
            let (dir, tar) = (dir.path(), &tar);
            s.spawn(move || {
                for i in 0..10 {
                    extract(tar, &dir.join(format!("{t}-{i}"))).unwrap();
                }
            });
        }
    });
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 80);
}

#[test]
fn a_staging_name_is_not_given_twice() {
    // One destination, with the first directory still there: only the counter tells them apart.
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("out");
    let (first, second) = (staging(&dest).unwrap(), staging(&dest).unwrap());
    assert_ne!(first, second);
    assert!(first.is_dir() && second.is_dir());
}

#[test]
fn parallel_extractions_of_one_destination_leave_one_tree() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("out");
    let tar = TarBuilder::default().file("a/b", b"x").gz();
    let start = std::sync::Barrier::new(8);
    let results: Vec<_> = std::thread::scope(|s| {
        let threads: Vec<_> = (0..8)
            .map(|_| {
                s.spawn(|| {
                    start.wait();
                    extract(&tar, &dest)
                })
            })
            .collect();
        threads.into_iter().map(|t| t.join().unwrap()).collect()
    });
    let won = results.iter().filter(|r| r.is_ok()).count();
    assert_eq!(won, 1, "{results:?}");
    assert_eq!(std::fs::read_to_string(dest.join("a/b")).unwrap(), "x");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}
