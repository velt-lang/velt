//! The package manager's archive extractor (`vpm::extract`): it never panics, and what it accepts
//! is a tree of clean relative paths whose link targets are relative.
//!
//! The first byte picks the input: even, the rest is a tar archive, gzip-compressed here (random
//! bytes rarely get past gzip's CRC, so the tar reader would see little); odd, the rest is the
//! archive as downloaded (a zip, or a .tar.gz).

use std::io::Write;

use vpm::extract::{read, Limits, Node};

const LIMITS: Limits = Limits {
    total: 4 << 20,
    entries: 2000,
    ratio: 1000,
};

pub fn check(data: &[u8]) {
    let Some((&mode, rest)) = data.split_first() else {
        return;
    };
    let archive = if mode % 2 == 0 {
        gz(rest)
    } else {
        rest.to_vec()
    };
    let Ok(tree) = read(&archive, LIMITS) else {
        return;
    };
    for (path, node) in tree.nodes() {
        assert!(!path.starts_with('/'), "absolute path {path:?}");
        for segment in path.split('/') {
            assert!(
                !matches!(segment, "" | "." | ".."),
                "segment {segment:?} in {path:?}"
            );
            assert!(
                !segment.contains(['\\', ':', '\0']),
                "name {segment:?} in {path:?}"
            );
        }
        if let Node::Link(target) = node {
            assert!(
                !target.is_empty() && !target.starts_with('/') && !target.contains(['\\', ':']),
                "link target {target:?} at {path:?}"
            );
        }
    }
    assert!(tree.bytes() <= LIMITS.total);
}

fn gz(bytes: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::GzEncoder::new(vec![], flate2::Compression::fast());
    e.write_all(bytes).unwrap();
    e.finish().unwrap()
}

#[cfg(test)]
mod tests {
    #[test]
    fn known_inputs() {
        super::check(b"");
        super::check(b"\0");
        super::check(b"\x01PK\x05\x06\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0");
        let mut tar = vec![0u8];
        tar.extend_from_slice(&[0; 1024]);
        super::check(&tar);
    }
}
