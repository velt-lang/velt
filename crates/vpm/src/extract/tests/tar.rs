//! The tar reader: headers, pax and GNU names, entry types, damage.

use super::*;

#[test]
fn long_names_come_from_pax_gnu_and_ustar_prefixes() {
    let long = format!("{}/file.c", "d".repeat(150));
    let pax = TarBuilder::default()
        .pax(b'x', &[("mtime", "1.5"), ("path", &long)])
        .file("short", b"x")
        .gz();
    assert_eq!(nodes(&pax)[1].0, long);
    let mut gnu_name = long.clone().into_bytes();
    gnu_name.push(0);
    let gnu = TarBuilder::default()
        .raw(b'L', "././@LongLink", &gnu_name, "")
        .file("short", b"x")
        .gz();
    assert_eq!(nodes(&gnu)[1].0, long);
    // A long link target through `K`.
    let target = format!("{}/x", "t".repeat(120));
    let k = TarBuilder::default()
        .raw(b'K', "././@LongLink", target.as_bytes(), "")
        .symlink("l", "short")
        .gz();
    assert_eq!(nodes(&k), [("l".to_string(), Node::Link(target))]);
    // ustar's prefix field.
    let mut h = header(b'0', b"name.c", 0, "");
    h[345..348].copy_from_slice(b"pre");
    set_checksum(&mut h);
    let mut raw = h.to_vec();
    raw.extend_from_slice(&[0; 1024]);
    assert_eq!(nodes(&gz(&raw))[1].0, "pre/name.c");
}

#[test]
fn a_global_pax_header_is_ignored() {
    // `git archive` writes the commit in one.
    let git = TarBuilder::default()
        .pax(b'g', &[("comment", "0123abcd")])
        .file("a", b"")
        .gz();
    assert_eq!(nodes(&git).len(), 1);
    // A path in one renames nothing: only an entry's own (`x`) header does.
    let named = TarBuilder::default()
        .pax(b'g', &[("path", "elsewhere")])
        .file("a", b"")
        .gz();
    assert_eq!(nodes(&named), [("a".to_string(), Node::File(vec![]))]);
    let sized = TarBuilder::default()
        .pax(b'x', &[("size", "99999999999")])
        .file("a", b"")
        .gz();
    refused(&sized, "a size in a pax header");
    let sparse = TarBuilder::default()
        .pax(b'x', &[("GNU.sparse.map", "0,1")])
        .file("a", b"")
        .gz();
    refused(&sparse, "a sparse file");
    let bad = TarBuilder::default()
        .raw(b'x', "pax", b"99 path=x\n", "")
        .file("a", b"")
        .gz();
    refused(&bad, "a malformed pax header");
}

#[test]
fn devices_fifos_and_unknown_entries_are_refused() {
    for (kind, why) in [
        (b'3', "a device file"),
        (b'4', "a device file"),
        (b'6', "a FIFO"),
        (b'S', "a sparse file"),
        (b'V', "an entry of unknown type 'V'"),
    ] {
        let tar = TarBuilder::default().raw(kind, "dev", b"", "").gz();
        let e = refusal(&tar);
        assert!(e.contains(why), "{}: expected {why:?} in {e}", kind as char);
    }
    // A contiguous file (`7`) is a file.
    let contiguous = TarBuilder::default().raw(b'7', "c", b"data", "").gz();
    assert_eq!(nodes(&contiguous)[0].1, Node::File(b"data".to_vec()));
}

#[test]
fn damaged_archives_are_refused() {
    let good = TarBuilder::default().file("a.txt", b"hello").tar();
    // A header byte changed: its checksum no longer matches.
    let mut flipped = good.clone();
    flipped[0] = b'b';
    refused(&gz(&flipped), "checksum is wrong");
    // Cut inside the data, inside a header, and before the end marker.
    refused(&gz(&good[..600]), "ends inside an entry");
    refused(&gz(&good[..100]), "ends inside a block");
    refused(&gz(&good[..1024]), "ends without its end marker");
    // A gzip stream cut short.
    let packed = gz(&good);
    refused(&packed[..packed.len() - 10], "not a valid .tar.gz");
    // Data after the end marker.
    let mut trailing = good.clone();
    trailing.extend_from_slice(&header(b'0', b"hidden", 0, ""));
    refused(&gz(&trailing), "data after the end of the tar archive");
    // A header that names no entry.
    let pending = TarBuilder::default().pax(b'x', &[("path", "a")]).gz();
    refused(&pending, "a header that names no entry");
    // Base-256 sizes.
    let mut huge = header(b'0', b"big", 0, "");
    huge[124] = 0x80;
    set_checksum(&mut huge);
    refused(&gz(&huge), "base-256");
}

#[test]
fn concatenated_gzip_members_are_one_stream() {
    let tar = TarBuilder::default().file("a", b"1").file("b", b"2").tar();
    let mut two = gz(&tar[..512 * 2]);
    two.extend(gz(&tar[512 * 2..]));
    assert_eq!(nodes(&two).len(), 2);
}

#[test]
fn archives_from_the_tar_crate_read_back() {
    // What GNU tar, bsdtar and git archive write is built by `tar` here: ustar and GNU headers,
    // long names, links.
    let mut b = ::tar::Builder::new(vec![]);
    let long = format!("pkg/{}/mod.velt", "n".repeat(120));
    for (path, data) in [
        ("pkg/package.velt", &b"export const pkg = {}"[..]),
        (&long[..], b"x"),
    ] {
        let mut h = ::tar::Header::new_gnu();
        h.set_size(data.len() as u64);
        h.set_mode(0o755);
        b.append_data(&mut h, path, data).unwrap();
    }
    let mut h = ::tar::Header::new_ustar();
    h.set_entry_type(::tar::EntryType::Symlink);
    h.set_size(0);
    b.append_link(&mut h, "pkg/link", "package.velt").unwrap();
    let archive = gz(&b.into_inner().unwrap());
    let got = nodes(&archive);
    assert!(got.contains(&(long, Node::File(b"x".to_vec()))));
    assert!(got.contains(&("pkg/link".to_string(), Node::Link("package.velt".into()))));
}
