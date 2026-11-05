//! Damaged and hostile variations of valid archives never panic, and whatever is accepted still
//! obeys the rules. A quick version of the `extract` fuzz target (fuzz/), run with every test.

use super::super::names;
use super::*;

/// A small deterministic generator (xorshift), so a failure reproduces.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

/// Change a few bytes of `bytes`: flips, interesting values, cuts, copies.
fn mutate(rng: &mut Rng, bytes: &[u8]) -> Vec<u8> {
    let mut out = bytes.to_vec();
    for _ in 0..1 + rng.below(4) {
        let at = rng.below(out.len());
        match rng.below(6) {
            0 => out[at] ^= 1 << rng.below(8),
            1 => out[at] = [0, 0xff, b'/', b'.', b'\\', b':', 0x7f][rng.below(7)],
            2 => out.truncate(at.max(1)),
            3 => {
                let len = rng.below(64).min(out.len() - at);
                let chunk = out[at..at + len].to_vec();
                let to = rng.below(out.len());
                out.splice(to..to, chunk);
            }
            4 => {
                let to = rng.below(out.len().saturating_sub(4));
                let end = (to + 4).min(out.len());
                out[to..end].fill(0xff);
            }
            _ => out[at] = out[at].wrapping_add(1),
        }
    }
    out
}

/// Whatever `read` accepts is clean, relative, and its links resolve inside.
fn check(archive: &[u8]) {
    let limits = Limits {
        total: 1 << 20,
        entries: 1000,
        ratio: 1000,
        names: 1 << 20,
    };
    let Ok(tree) = read(archive, limits) else {
        return;
    };
    for (path, node) in tree.nodes() {
        assert_eq!(names::clean(path).unwrap().as_deref(), Some(path));
        if let Node::Link(target) = node {
            names::check_link_target(target).unwrap();
        }
    }
}

fn seeds() -> (Vec<u8>, Vec<u8>) {
    let tar = TarBuilder::default()
        .pax(b'x', &[("path", "pkg/long/name.vlt")])
        .file("ignored", b"pax named")
        .dir("pkg/")
        .file("pkg/a.vlt", b"export const a = 1")
        .symlink("pkg/l", "a.vlt")
        .hardlink("pkg/h", "pkg/a.vlt")
        .tar();
    let zip = zip(
        &[
            ZipEntry::file("pkg/", b""),
            ZipEntry::file("pkg/a.vlt", &b"export const a = 1;\n".repeat(20)),
            ZipEntry::symlink("pkg/l", "a.vlt"),
        ],
        true,
    );
    (tar, zip)
}

#[test]
fn mutated_archives_never_panic() {
    let (tar, zip) = seeds();
    let mut rng = Rng(0x2367_5eed);
    for _ in 0..3000 {
        // The tar is mutated before compressing: a damaged gzip stream rarely gets further.
        check(&gz(&mutate(&mut rng, &tar)));
        check(&mutate(&mut rng, &zip));
    }
    check(&mutate(&mut rng, &gz(&tar)));
}
