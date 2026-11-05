//! Unpacked size, entry count and compression ratio, enforced while decompressing.

use super::*;

const SMALL: Limits = Limits {
    total: 4 << 20,
    entries: 10,
    ratio: 200,
};

/// [`SMALL`] without the ratio, for archives of repeated bytes.
const ANY_RATIO: Limits = Limits {
    ratio: 100_000,
    ..SMALL
};

#[test]
fn the_unpacked_size_is_capped() {
    let big = vec![b'x'; 3 << 20];
    let two = TarBuilder::default().file("a", &big).file("b", &big).gz();
    let e = read(&two, SMALL).unwrap_err();
    assert!(e.contains("more than"), "{e}");
    let one = TarBuilder::default().file("a", &big).gz();
    assert!(read(&one, ANY_RATIO).is_ok());
    let zip_two = zip(
        &[ZipEntry::file("a", &big), ZipEntry::file("b", &big)],
        false,
    );
    assert!(read(&zip_two, ANY_RATIO).is_err());
    // A hard link counts as the copy it becomes.
    let linked = TarBuilder::default()
        .file("a", &big)
        .hardlink("b", "a")
        .gz();
    let e = read(&linked, ANY_RATIO).unwrap_err();
    assert!(e.contains("more than 4 MiB"), "{e}");
}

#[test]
fn a_tar_header_cannot_announce_more_than_is_allowed() {
    // A header claiming 1 GiB is refused before anything is allocated or read.
    let mut h = header(b'0', b"huge", 1 << 30, "");
    set_checksum(&mut h);
    let e = read(&gz(&h), SMALL).unwrap_err();
    assert!(e.contains("more than"), "{e}");
}

#[test]
fn the_entry_count_is_capped() {
    let mut many = TarBuilder::default();
    for i in 0..11 {
        many = many.file(&format!("f{i}"), b"");
    }
    let e = read(&many.gz(), SMALL).unwrap_err();
    assert!(e.contains("more than 10 entries"), "{e}");
    let entries: Vec<ZipEntry> = (0..11)
        .map(|i| ZipEntry::file(&format!("f{i}"), b""))
        .collect();
    let e = read(&zip(&entries, false), SMALL).unwrap_err();
    assert!(e.contains("more than 10 entries"), "{e}");
}

#[test]
fn the_compression_ratio_is_capped() {
    // 3 MiB of zeros deflate to a few KiB: over 200 times past the first MiB.
    let zeros = vec![0u8; 3 << 20];
    let tar = TarBuilder::default().file("z", &zeros).gz();
    let e = read(&tar, SMALL).unwrap_err();
    assert!(e.contains("times its size"), "{e}");
    let zip = zip(&[ZipEntry::file("z", &zeros)], false);
    let e = read(&zip, SMALL).unwrap_err();
    assert!(e.contains("times its size"), "{e}");
    // The first MiB is free: small archives of repetitive text are fine.
    let small = TarBuilder::default().file("z", &[0u8; 500 << 10]).gz();
    assert!(read(&small, SMALL).is_ok());
}

#[test]
fn zeros_after_the_end_marker_count_too() {
    // A tar whose end is followed by a gzip bomb of zero blocks.
    let mut tar = TarBuilder::default().file("a", b"x").tar();
    tar.resize(tar.len() + (8 << 20), 0);
    let e = read(&gz(&tar), SMALL).unwrap_err();
    assert!(
        e.contains("times its size") || e.contains("more than"),
        "{e}"
    );
}
