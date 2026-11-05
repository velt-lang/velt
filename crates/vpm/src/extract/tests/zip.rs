//! The zip reader: central directory against local headers, overlaps, entry types, damage.

use super::*;

fn find(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .position(|w| w == needle)
        .unwrap()
}

#[test]
fn stored_deflated_and_described_entries_read_back() {
    let mut stored = ZipEntry::file("dir/s.txt", b"stored bytes");
    stored.method = 0;
    let entries = [
        stored,
        ZipEntry::file("dir/d.txt", &b"deflated ".repeat(100)),
        ZipEntry::file("empty/", b""),
    ];
    for descriptor in [false, true] {
        let got = nodes(&zip(&entries, descriptor));
        assert_eq!(got.len(), 4, "{got:?}");
        assert!(got.contains(&("dir/s.txt".into(), Node::File(b"stored bytes".to_vec()))));
        assert!(got.contains(&("empty".into(), Node::Dir)));
    }
    // An empty zip is an empty tree.
    assert!(nodes(&zip(&[], false)).is_empty());
}

#[test]
fn a_local_header_that_disagrees_is_refused() {
    let good = zip(&[ZipEntry::file("a.txt", b"hello")], false);
    // The local name says `b.txt`; the central directory `a.txt`.
    let mut renamed = good.clone();
    renamed[30] = b'b';
    refused(
        &renamed,
        "its local header disagrees with the central directory (name)",
    );
    let mut method = good.clone();
    method[8] = 0;
    refused(&method, "(compression method)");
    let mut flags = good.clone();
    flags[6] = 1 << 1;
    refused(&flags, "(flags)");
    let mut size = good.clone();
    size[22] ^= 1;
    refused(&size, "(CRC or sizes)");
    // A data descriptor that disagrees.
    let mut described = zip(&[ZipEntry::file("a.txt", b"hello")], true);
    let at = find(&described, &0x0807_4b50u32.to_le_bytes());
    described[at + 8] ^= 1;
    refused(&described, "its data descriptor disagrees");
}

/// The local part (headers and data) and the central entries of a zip built by [`zip`].
fn split(z: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let cd = find(z, &0x0201_4b50u32.to_le_bytes());
    let eocd = find(z, &0x0605_4b50u32.to_le_bytes());
    (z[..cd].to_vec(), z[cd..eocd].to_vec())
}

/// A zip of `local` and `centrals` (entry count 1 each).
fn assemble(local: &[u8], centrals: &[Vec<u8>]) -> Vec<u8> {
    let mut out = local.to_vec();
    let central: Vec<u8> = centrals.concat();
    out.extend_from_slice(&central);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    let n = (centrals.len() as u16).to_le_bytes();
    out.extend_from_slice(&n);
    out.extend_from_slice(&n);
    out.extend_from_slice(&(central.len() as u32).to_le_bytes());
    out.extend_from_slice(&(local.len() as u32).to_le_bytes());
    out.extend_from_slice(&[0, 0]);
    out
}

#[test]
fn entries_that_share_data_are_refused() {
    // `b`'s local header and data sit inside `a`'s stored data, and both central entries point
    // at their own: each is valid alone, together they overlap (the overlapping-files bomb).
    let (local_b, central_b) = split(&zip(&[ZipEntry::file("b", b"payload")], false));
    let mut a = ZipEntry::file("a", &local_b);
    a.method = 0;
    let (local_a, central_a) = split(&zip(&[a], false));
    let mut inner = central_b.clone();
    // `b`'s local header starts after `a`'s header (30 bytes) and name (1 byte).
    inner[42..46].copy_from_slice(&31u32.to_le_bytes());
    refused(
        &assemble(&local_a, &[central_a.clone(), inner]),
        "two entries of the zip share data",
    );
    // The same entries without the overlap read back.
    assert_eq!(nodes(&assemble(&local_a, &[central_a])).len(), 1);
}

#[test]
fn encrypted_unknown_and_zip64_entries_are_refused() {
    let mut encrypted = ZipEntry::file("a", b"x");
    encrypted.flags = 1;
    refused(&zip(&[encrypted], false), "an encrypted entry");
    let mut lzma = ZipEntry::file("a", b"x");
    lzma.method = 14;
    refused(&zip(&[lzma], false), "compression method 14");
    // A zip64 end locator before the end record.
    let mut z64 = zip(&[ZipEntry::file("a", b"x")], false);
    let eocd = find(&z64, &0x0605_4b50u32.to_le_bytes());
    let mut locator = 0x0706_4b50u32.to_le_bytes().to_vec();
    locator.resize(20, 0);
    z64.splice(eocd..eocd, locator);
    refused(&z64, "a zip64 archive");
}

#[test]
fn unix_types_in_the_attributes_are_honoured() {
    for (mode, why) in [
        (0o010644, "a FIFO"),
        (0o020644, "a device file"),
        (0o060644, "a device file"),
        (0o140644, "a socket"),
        (0o040755, "a directory whose name doesn't end in `/`"),
    ] {
        let mut e = ZipEntry::file("x", b"");
        e.mode = Some(mode);
        let err = refusal(&zip(&[e], false));
        assert!(err.contains(why), "{mode:o}: expected {why:?} in {err}");
    }
    let mut exec = ZipEntry::file("run.sh", b"#!/bin/sh");
    exec.mode = Some(0o100755);
    assert_eq!(
        nodes(&zip(&[exec], false))[0].1,
        Node::File(b"#!/bin/sh".to_vec())
    );
    refused(
        &zip(&[ZipEntry::file("d/", b"data")], false),
        "a directory with data",
    );
}

#[test]
fn damaged_zips_are_refused() {
    let good = zip(&[ZipEntry::file("a.txt", &b"hello ".repeat(50))], false);
    // Truncated: no end record.
    refused(
        &good[..good.len() - 5],
        "no end of central directory record",
    );
    // Data prepended (a self-extracting stub).
    let mut stub = b"MZ stub".to_vec();
    stub.extend_from_slice(&good);
    stub[..2].copy_from_slice(b"PK");
    stub[2] = 3;
    stub[3] = 4;
    refused(&stub, "data prepended or inserted");
    // A comment that doesn't reach the end.
    let mut comment = good.clone();
    let n = comment.len();
    comment[n - 2] = 9;
    refused(&comment, "no end of central directory record");
    // Compressed data damaged: the CRC or the deflate stream fails.
    let mut bad = good.clone();
    bad[40] ^= 0xff;
    let e = refusal(&bad);
    assert!(
        e.contains("damaged") || e.contains("CRC") || e.contains("size"),
        "{e}"
    );
    // The stored CRC wrong.
    let mut crc = zip(&[ZipEntry::file("a", b"x")], false);
    let cd = find(&crc, &0x0201_4b50u32.to_le_bytes());
    crc[14] ^= 1;
    crc[cd + 16] ^= 1;
    refused(&crc, "its CRC is wrong");
}

#[test]
fn archives_from_the_zip_crate_read_back() {
    let mut w = ::zip::ZipWriter::new(std::io::Cursor::new(vec![]));
    let options = ::zip::write::SimpleFileOptions::default()
        .compression_method(::zip::CompressionMethod::Deflated)
        .unix_permissions(0o644);
    w.add_directory("sqlite/", options).unwrap();
    w.start_file("sqlite/sqlite3.c", options).unwrap();
    std::io::Write::write_all(&mut w, &b"/* amalgamation */\n".repeat(1000)).unwrap();
    w.add_symlink("sqlite/latest.c", "sqlite3.c", options)
        .unwrap();
    let archive = w.finish().unwrap().into_inner();
    let got = nodes(&archive);
    assert_eq!(got.len(), 3, "{got:?}");
    assert_eq!(
        got[2],
        (
            "sqlite/sqlite3.c".into(),
            Node::File(b"/* amalgamation */\n".repeat(1000))
        )
    );
    assert_eq!(
        got[1],
        ("sqlite/latest.c".into(), Node::Link("sqlite3.c".into()))
    );
}
