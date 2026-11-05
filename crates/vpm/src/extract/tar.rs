//! `.tar.gz`: a gzip stream (several members, as `cat a.gz b.gz` gives, read as one) holding a
//! ustar, pax or GNU tar archive. Read as a stream, so the limits hold while decompressing:
//! no file is allocated before its size is checked against what is left.
//!
//! Accepted: files, directories, symbolic and hard links; pax headers (`path` and `linkpath`
//! taken, `size` and sparse keys refused, the rest ignored, global ones too: `git archive` writes
//! its commit in one); GNU long names (`L`, `K`). Refused: devices, FIFOs, sparse files, sizes in
//! base-256, a header whose checksum is wrong, and anything after the archive ends but zeros.

use std::io::Read;

use flate2::read::MultiGzDecoder;

use super::tree::{Entry, Tree};
use super::Limits;

const BLOCK: usize = 512;
/// The most a pax header or a GNU long name may be.
const MAX_META: u64 = 1 << 20;

/// Add the entries of the `.tar.gz` `archive` to `tree`.
pub(super) fn read(archive: &[u8], limits: Limits, tree: &mut Tree) -> Result<(), String> {
    let mut stream = Stream {
        gz: MultiGzDecoder::new(archive),
        read: 0,
        packed: archive.len(),
        limits,
    };
    let mut next = Pending::default();
    loop {
        let Some(header) = stream.block()? else {
            return Err("the tar archive ends without its end marker".to_string());
        };
        if header.iter().all(|&b| b == 0) {
            return stream.rest_is_zeros(next);
        }
        let header = Header::parse(&header)?;
        match header.kind {
            b'x' => next.pax(&stream.meta(header.size)?)?,
            b'g' => Pending::default().pax(&stream.meta(header.size)?)?,
            b'L' => next.path = Some(nul_terminated(&stream.meta(header.size)?)?),
            b'K' => next.link = Some(nul_terminated(&stream.meta(header.size)?)?),
            kind => {
                let pending = std::mem::take(&mut next);
                let path = pending.path.unwrap_or(header.path);
                let link = pending.link.unwrap_or(header.link);
                let entry = match kind {
                    b'0' | b'\0' | b'7' => Entry::File(stream.file(header.size)?),
                    b'5' => Entry::Dir,
                    b'2' => Entry::Symlink(link),
                    b'1' => Entry::HardLink(link),
                    _ => return Err(format!("`{}`: {}", path.escape_debug(), refused(kind))),
                };
                if !matches!(kind, b'0' | b'\0' | b'7') {
                    stream.skip(header.size)?;
                }
                tree.add(&path, entry, archive.len())?;
            }
        }
    }
}

fn refused(kind: u8) -> String {
    match kind {
        b'3' | b'4' => "a device file".to_string(),
        b'6' => "a FIFO".to_string(),
        b'S' => "a sparse file".to_string(),
        kind => format!("an entry of unknown type {:?}", kind as char),
    }
}

/// What the headers before an entry say about it.
#[derive(Default)]
struct Pending {
    path: Option<String>,
    link: Option<String>,
}

impl Pending {
    /// Take a pax header's records: `<length> <key>=<value>\n`.
    fn pax(&mut self, data: &[u8]) -> Result<(), String> {
        let mut rest = data;
        while !rest.is_empty() {
            let bad = || "a malformed pax header".to_string();
            let space = rest.iter().position(|&b| b == b' ').ok_or_else(bad)?;
            let len: usize = std::str::from_utf8(&rest[..space])
                .ok()
                .filter(|s| s.bytes().all(|b| b.is_ascii_digit()))
                .and_then(|s| s.parse().ok())
                .ok_or_else(bad)?;
            if len <= space + 1 || len > rest.len() || rest[len - 1] != b'\n' {
                return Err(bad());
            }
            let record = &rest[space + 1..len - 1];
            let eq = record.iter().position(|&b| b == b'=').ok_or_else(bad)?;
            let value = || utf8(&record[eq + 1..]);
            match &record[..eq] {
                b"path" => self.path = Some(value()?),
                b"linkpath" => self.link = Some(value()?),
                b"size" => return Err("a size in a pax header (a file over 8 GiB)".to_string()),
                key if key.starts_with(b"GNU.sparse.") => return Err("a sparse file".to_string()),
                _ => {}
            }
            rest = &rest[len..];
        }
        Ok(())
    }
}

/// The parts of a header block the extractor uses.
struct Header {
    path: String,
    link: String,
    size: u64,
    kind: u8,
}

impl Header {
    fn parse(b: &[u8; BLOCK]) -> Result<Header, String> {
        let stored = octal(&b[148..156]).ok_or("a tar header with a malformed checksum")?;
        let sum: u64 = (b.iter().enumerate())
            .map(|(i, &x)| {
                if (148..156).contains(&i) {
                    32
                } else {
                    x as u64
                }
            })
            .sum();
        if sum != stored {
            return Err("a tar header whose checksum is wrong (a damaged archive?)".to_string());
        }
        if b[124] & 0x80 != 0 {
            return Err("a size in base-256 (a file over 8 GiB)".to_string());
        }
        let size = octal(&b[124..136]).ok_or("a tar header with a malformed size")?;
        let name = utf8(field(&b[0..100]))?;
        // ustar's prefix; GNU tar (`ustar  \0`) keeps other fields there.
        let path = match &b[257..263] == b"ustar\0" && b[345] != 0 {
            true => format!("{}/{name}", utf8(field(&b[345..500]))?),
            false => name,
        };
        Ok(Header {
            path,
            link: utf8(field(&b[157..257]))?,
            size,
            kind: b[156],
        })
    }
}

/// A NUL-padded field.
fn field(b: &[u8]) -> &[u8] {
    &b[..b.iter().position(|&x| x == 0).unwrap_or(b.len())]
}

/// An octal number, padded with spaces or NULs.
fn octal(b: &[u8]) -> Option<u64> {
    let text = std::str::from_utf8(b).ok()?;
    let digits = text.trim_matches(|c| c == ' ' || c == '\0');
    if digits.is_empty() {
        return Some(0);
    }
    u64::from_str_radix(digits, 8).ok()
}

fn utf8(b: &[u8]) -> Result<String, String> {
    String::from_utf8(b.to_vec()).map_err(|_| "a name that is not UTF-8".to_string())
}

fn nul_terminated(b: &[u8]) -> Result<String, String> {
    utf8(field(b))
}

/// The decompressed archive, read block by block.
struct Stream<'a> {
    gz: MultiGzDecoder<&'a [u8]>,
    /// Decompressed bytes read so far.
    read: u64,
    packed: usize,
    limits: Limits,
}

impl Stream<'_> {
    fn fail(e: std::io::Error) -> String {
        format!("it is not a valid .tar.gz: {e}")
    }

    /// The next block, or `None` at the end of the stream.
    fn block(&mut self) -> Result<Option<[u8; BLOCK]>, String> {
        let mut block = [0; BLOCK];
        let mut filled = 0;
        while filled < BLOCK {
            match self.gz.read(&mut block[filled..]).map_err(Self::fail)? {
                0 if filled == 0 => return Ok(None),
                0 => return Err("the tar archive ends inside a block".to_string()),
                n => filled += n,
            }
        }
        self.count(BLOCK as u64)?;
        Ok(Some(block))
    }

    /// Count `n` more decompressed bytes against the limits: the stream as a whole is capped,
    /// headers and padding too, not only the files.
    fn count(&mut self, n: u64) -> Result<(), String> {
        self.read = self.read.saturating_add(n);
        let headers = (self.limits.entries as u64).saturating_mul(3 * BLOCK as u64);
        let limits = Limits {
            total: self.limits.total.saturating_add(headers),
            ..self.limits
        };
        limits.check_size(self.read, self.packed)
    }

    /// The `size` bytes of an entry, and its padding.
    fn data(&mut self, size: u64, max: u64) -> Result<Vec<u8>, String> {
        if size > max {
            return Err(format!("an entry of {size} bytes, more than allowed"));
        }
        let padded = size.div_ceil(BLOCK as u64) * BLOCK as u64;
        self.count(padded)?;
        let mut bytes = Vec::with_capacity(padded as usize);
        (&mut self.gz)
            .take(padded)
            .read_to_end(&mut bytes)
            .map_err(Self::fail)?;
        if bytes.len() as u64 != padded {
            return Err("the tar archive ends inside an entry".to_string());
        }
        bytes.truncate(size as usize);
        Ok(bytes)
    }

    fn file(&mut self, size: u64) -> Result<Vec<u8>, String> {
        self.data(size, self.limits.total)
    }

    fn meta(&mut self, size: u64) -> Result<Vec<u8>, String> {
        self.data(size, MAX_META)
    }

    fn skip(&mut self, size: u64) -> Result<(), String> {
        self.data(size, MAX_META).map(drop)
    }

    /// After the first zero block: the rest must be zeros, and no header may wait for an entry.
    fn rest_is_zeros(&mut self, pending: Pending) -> Result<(), String> {
        if pending.path.is_some() || pending.link.is_some() {
            return Err("the tar archive ends after a header that names no entry".to_string());
        }
        while let Some(block) = self.block()? {
            if block.iter().any(|&b| b != 0) {
                return Err("data after the end of the tar archive".to_string());
            }
        }
        Ok(())
    }
}
