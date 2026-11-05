//! Archives built byte by byte, so each refusal can be given exactly the input it is about.

mod limits;
mod links;
mod mutate;
mod names;
mod tar;
mod write;
mod zip;

use std::io::Write as _;

use super::*;

/// A tar archive, one header at a time.
#[derive(Default)]
pub(super) struct TarBuilder {
    out: Vec<u8>,
}

impl TarBuilder {
    pub(super) fn file(self, name: &str, bytes: &[u8]) -> Self {
        self.raw(b'0', name, bytes, "")
    }

    pub(super) fn dir(self, name: &str) -> Self {
        self.raw(b'5', name, b"", "")
    }

    pub(super) fn symlink(self, name: &str, target: &str) -> Self {
        self.raw(b'2', name, b"", target)
    }

    pub(super) fn hardlink(self, name: &str, target: &str) -> Self {
        self.raw(b'1', name, b"", target)
    }

    /// A pax header (`x`, or `g` for a global one) with these records.
    pub(super) fn pax(self, kind: u8, records: &[(&str, &str)]) -> Self {
        let mut data = vec![];
        for (key, value) in records {
            let body = format!(" {key}={value}\n");
            // The length counts its own digits.
            let mut len = body.len() + 1;
            while format!("{len}").len() + body.len() != len {
                len += 1;
            }
            data.extend_from_slice(format!("{len}{body}").as_bytes());
        }
        self.raw(kind, "pax", &data, "")
    }

    /// An entry of any type with this data.
    pub(super) fn raw(mut self, kind: u8, name: &str, data: &[u8], link: &str) -> Self {
        self.out
            .extend_from_slice(&header(kind, name.as_bytes(), data.len() as u64, link));
        self.out.extend_from_slice(data);
        self.out.resize(self.out.len().div_ceil(512) * 512, 0);
        self
    }

    /// The archive with its end marker.
    pub(super) fn tar(mut self) -> Vec<u8> {
        self.out.extend_from_slice(&[0; 1024]);
        self.out
    }

    pub(super) fn gz(self) -> Vec<u8> {
        gz(&self.tar())
    }
}

/// A ustar header.
pub(super) fn header(kind: u8, name: &[u8], size: u64, link: &str) -> [u8; 512] {
    let mut h = [0u8; 512];
    h[..name.len()].copy_from_slice(name);
    h[100..108].copy_from_slice(b"0000644\0");
    h[108..116].copy_from_slice(b"0000000\0");
    h[116..124].copy_from_slice(b"0000000\0");
    h[124..136].copy_from_slice(format!("{size:011o}\0").as_bytes());
    h[136..148].copy_from_slice(b"00000000000\0");
    h[156] = kind;
    h[157..157 + link.len()].copy_from_slice(link.as_bytes());
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    set_checksum(&mut h);
    h
}

pub(super) fn set_checksum(h: &mut [u8; 512]) {
    h[148..156].copy_from_slice(b"        ");
    let sum: u32 = h.iter().map(|&b| b as u32).sum();
    h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
}

pub(super) fn gz(bytes: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::GzEncoder::new(vec![], flate2::Compression::default());
    e.write_all(bytes).unwrap();
    e.finish().unwrap()
}

/// One zip entry.
#[derive(Clone)]
pub(super) struct ZipEntry {
    pub name: Vec<u8>,
    pub data: Vec<u8>,
    /// 0 (stored) or 8 (deflate).
    pub method: u16,
    pub flags: u16,
    /// A Unix mode in the external attributes (made by Unix).
    pub mode: Option<u32>,
}

impl ZipEntry {
    pub(super) fn file(name: &str, data: &[u8]) -> ZipEntry {
        ZipEntry {
            name: name.as_bytes().to_vec(),
            data: data.to_vec(),
            method: 8,
            flags: 0,
            mode: None,
        }
    }

    pub(super) fn symlink(name: &str, target: &str) -> ZipEntry {
        ZipEntry {
            mode: Some(0o120777),
            method: 0,
            ..ZipEntry::file(name, target.as_bytes())
        }
    }
}

/// A zip archive of `entries`; with `descriptor`, sizes and CRC follow each entry's data.
pub(super) fn zip(entries: &[ZipEntry], descriptor: bool) -> Vec<u8> {
    let mut out = vec![];
    let mut central = vec![];
    for e in entries {
        let mut crc = flate2::Crc::new();
        crc.update(&e.data);
        let crc = crc.sum();
        let packed = match e.method {
            8 => {
                let mut d = flate2::write::DeflateEncoder::new(vec![], flate2::Compression::best());
                d.write_all(&e.data).unwrap();
                d.finish().unwrap()
            }
            _ => e.data.clone(),
        };
        let flags = e.flags | if descriptor { 1 << 3 } else { 0 };
        let offset = out.len() as u32;
        let sizes = |out: &mut Vec<u8>| {
            out.extend_from_slice(&crc.to_le_bytes());
            out.extend_from_slice(&(packed.len() as u32).to_le_bytes());
            out.extend_from_slice(&(e.data.len() as u32).to_le_bytes());
        };
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        out.extend_from_slice(&[20, 0]);
        out.extend_from_slice(&flags.to_le_bytes());
        out.extend_from_slice(&e.method.to_le_bytes());
        out.extend_from_slice(&[0; 4]);
        match descriptor {
            true => out.extend_from_slice(&[0; 12]),
            false => sizes(&mut out),
        }
        out.extend_from_slice(&(e.name.len() as u16).to_le_bytes());
        out.extend_from_slice(&[0, 0]);
        out.extend_from_slice(&e.name);
        out.extend_from_slice(&packed);
        if descriptor {
            out.extend_from_slice(&0x0807_4b50u32.to_le_bytes());
            sizes(&mut out);
        }
        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        let made_by: u16 = if e.mode.is_some() { 0x031e } else { 0x0014 };
        central.extend_from_slice(&made_by.to_le_bytes());
        central.extend_from_slice(&[20, 0]);
        central.extend_from_slice(&flags.to_le_bytes());
        central.extend_from_slice(&e.method.to_le_bytes());
        central.extend_from_slice(&[0; 4]);
        sizes(&mut central);
        central.extend_from_slice(&(e.name.len() as u16).to_le_bytes());
        central.extend_from_slice(&[0; 8]);
        central.extend_from_slice(&(e.mode.unwrap_or(0) << 16).to_le_bytes());
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(&e.name);
    }
    let cd_offset = out.len() as u32;
    out.extend_from_slice(&central);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    let n = (entries.len() as u16).to_le_bytes();
    out.extend_from_slice(&n);
    out.extend_from_slice(&n);
    out.extend_from_slice(&(central.len() as u32).to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(&[0, 0]);
    out
}

/// The tree in `archive`, as `(path, node)` pairs.
pub(super) fn nodes(archive: &[u8]) -> Vec<(String, Node)> {
    let tree = read(archive, Limits::DEFAULT).unwrap();
    tree.nodes()
        .map(|(p, n)| (p.to_string(), n.clone()))
        .collect()
}

/// The refusal of `archive`.
#[track_caller]
pub(super) fn refusal(archive: &[u8]) -> String {
    match read(archive, Limits::DEFAULT) {
        Ok(tree) => panic!("accepted: {:?}", tree.nodes().collect::<Vec<_>>()),
        Err(e) => e,
    }
}

/// `archive` is refused with a message containing `part`.
#[track_caller]
pub(super) fn refused(archive: &[u8], part: &str) {
    let e = refusal(archive);
    assert!(e.contains(part), "expected {part:?} in: {e}");
}

#[test]
fn the_format_comes_from_the_first_bytes() {
    let tar = TarBuilder::default().file("a.txt", b"hi").gz();
    let zip = zip(&[ZipEntry::file("a.txt", b"hi")], false);
    let expected = vec![("a.txt".to_string(), Node::File(b"hi".to_vec()))];
    assert_eq!(nodes(&tar), expected);
    assert_eq!(nodes(&zip), expected);
    refused(b"not an archive", "neither a .tar.gz nor a .zip");
    refused(b"", "neither");
    // A plain tar is not accepted: the format is gzip-compressed tar.
    refused(&TarBuilder::default().file("a", b"").tar(), "neither");
}
