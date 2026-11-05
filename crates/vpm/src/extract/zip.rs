//! `.zip`: the central directory is the list of entries, and each entry's local header must say
//! the same (name, method, flags, and sizes and CRC unless a data descriptor follows the data,
//! which must then say the same). Entries may not overlap: a zip whose entries share compressed
//! data is a known bomb. Stored and deflated entries only; encrypted, zip64 and multi-disk
//! archives, prepended data (a self-extracting stub) and comments that don't end the file are
//! refused. Names must be UTF-8 (flagged so, or plain ASCII): any other byte would be read as
//! CP437 by one tool and UTF-8 by another.

use flate2::read::DeflateDecoder;
use flate2::Crc;

use super::tree::{Entry, Tree};
use super::{read_capped, Limits};

const EOCD: u32 = 0x0605_4b50;
const ZIP64_LOCATOR: u32 = 0x0706_4b50;
const CENTRAL: u32 = 0x0201_4b50;
const LOCAL: u32 = 0x0403_4b50;
const DESCRIPTOR: u32 = 0x0807_4b50;

const FLAG_ENCRYPTED: u16 = 1;
const FLAG_DESCRIPTOR: u16 = 1 << 3;
const FLAG_STRONG_ENCRYPTION: u16 = 1 << 6;
const FLAG_UTF8: u16 = 1 << 11;
const FLAG_MASKED_HEADER: u16 = 1 << 13;

/// The longest link target read from an entry's data.
const MAX_LINK: u32 = 4096;

/// Add the entries of the `.zip` `archive` to `tree`.
pub(super) fn read(archive: &[u8], limits: Limits, tree: &mut Tree) -> Result<(), String> {
    let z = Zip { b: archive };
    let (count, cd_start, cd_end) = z.end_of_central_directory()?;
    let mut at = cd_start;
    let mut regions = vec![];
    let mut unpacked: u64 = 0;
    for _ in 0..count {
        let c = z.central(at)?;
        if c.next > cd_end {
            return Err("an entry of the zip's central directory runs past its end".to_string());
        }
        at = c.next;
        let name = c.name()?;
        let named = |why: String| format!("`{}`: {why}", name.escape_debug());
        unpacked = unpacked.saturating_add(c.size as u64);
        limits.check_size(unpacked, archive.len())?;
        let (start, end) = z.local(&c, cd_start).map_err(named)?;
        regions.push((c.offset as usize, end));
        let entry = z.entry(&c, &name, start).map_err(named)?;
        tree.add(&name, entry, archive.len())?;
    }
    if at != cd_end {
        return Err("the zip's central directory has data after its last entry".to_string());
    }
    regions.sort_unstable();
    if regions.windows(2).any(|w| w[1].0 < w[0].1) {
        return Err("two entries of the zip share data (a zip bomb?)".to_string());
    }
    Ok(())
}

/// One central directory entry.
struct Central<'a> {
    made_by: u16,
    flags: u16,
    method: u16,
    crc: u32,
    packed: u32,
    size: u32,
    name: &'a [u8],
    external: u32,
    offset: u32,
    /// Where the next entry starts.
    next: usize,
}

impl Central<'_> {
    fn name(&self) -> Result<String, String> {
        let text = String::from_utf8(self.name.to_vec());
        match text {
            Ok(name) if self.flags & FLAG_UTF8 != 0 || name.is_ascii() => Ok(name),
            _ => Err(format!(
                "`{}`: a name that is not flagged UTF-8 and is not ASCII (its encoding is \
                 ambiguous)",
                String::from_utf8_lossy(self.name).escape_debug()
            )),
        }
    }

    /// The Unix file type in the external attributes, when a Unix tool wrote them.
    fn unix_type(&self) -> Option<u32> {
        (self.made_by >> 8 == 3).then_some((self.external >> 16) & 0o170000)
    }
}

struct Zip<'a> {
    b: &'a [u8],
}

impl<'a> Zip<'a> {
    fn u16(&self, at: usize) -> Result<u16, String> {
        let s = self.bytes(at, 2)?;
        Ok(u16::from_le_bytes([s[0], s[1]]))
    }

    fn u32(&self, at: usize) -> Result<u32, String> {
        let s = self.bytes(at, 4)?;
        Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    }

    fn bytes(&self, at: usize, len: usize) -> Result<&'a [u8], String> {
        let end = at.checked_add(len).ok_or_else(Self::short)?;
        self.b.get(at..end).ok_or_else(Self::short)
    }

    fn short() -> String {
        "the zip ends inside a header (a truncated archive?)".to_string()
    }

    /// The entry count and the central directory's range.
    fn end_of_central_directory(&self) -> Result<(usize, usize, usize), String> {
        let len = self.b.len();
        // The record is 22 bytes and its comment ends the file: search backwards, at most a
        // 64 KiB comment, for the one whose comment length reaches exactly the end.
        let lowest = len.saturating_sub(22 + 0xffff);
        let at = (lowest..=len.saturating_sub(22))
            .rev()
            .find(|&i| {
                self.u32(i) == Ok(EOCD)
                    && self.u16(i + 20).is_ok_and(|c| i + 22 + c as usize == len)
            })
            .ok_or("the zip has no end of central directory record (truncated?)")?;
        if at >= 20 && self.u32(at - 20) == Ok(ZIP64_LOCATOR) {
            return Err("a zip64 archive (not supported)".to_string());
        }
        let (disk, cd_disk) = (self.u16(at + 4)?, self.u16(at + 6)?);
        let (here, count) = (self.u16(at + 8)?, self.u16(at + 10)?);
        let (cd_size, cd_offset) = (self.u32(at + 12)?, self.u32(at + 16)?);
        if count == 0xffff || cd_size == u32::MAX || cd_offset == u32::MAX {
            return Err("a zip64 archive (not supported)".to_string());
        }
        if disk != 0 || cd_disk != 0 || here != count {
            return Err("a zip split over several disks (not supported)".to_string());
        }
        let start = cd_offset as usize;
        if start.checked_add(cd_size as usize) != Some(at) {
            return Err(
                "the zip's central directory doesn't end where its end record starts (data \
                 prepended or inserted)"
                    .to_string(),
            );
        }
        Ok((count as usize, start, at))
    }

    fn central(&self, at: usize) -> Result<Central<'a>, String> {
        if self.u32(at)? != CENTRAL {
            return Err("a damaged zip central directory".to_string());
        }
        let name_len = self.u16(at + 28)? as usize;
        let extra_len = self.u16(at + 30)? as usize;
        let comment_len = self.u16(at + 32)? as usize;
        let c = Central {
            made_by: self.u16(at + 4)?,
            flags: self.u16(at + 8)?,
            method: self.u16(at + 10)?,
            crc: self.u32(at + 16)?,
            packed: self.u32(at + 20)?,
            size: self.u32(at + 24)?,
            name: self.bytes(at + 46, name_len)?,
            external: self.u32(at + 38)?,
            offset: self.u32(at + 42)?,
            next: [46, name_len, extra_len, comment_len]
                .iter()
                .try_fold(at, |sum, n| sum.checked_add(*n))
                .ok_or_else(Self::short)?,
        };
        let extra = self.bytes(at + 46 + name_len, extra_len)?;
        if self.u16(at + 34)? != 0 {
            return Err("a zip split over several disks (not supported)".to_string());
        }
        if c.packed == u32::MAX || c.size == u32::MAX || c.offset == u32::MAX || has_zip64(extra) {
            return Err("a zip64 entry (not supported)".to_string());
        }
        Ok(c)
    }

    /// Check the local header against `c`: where its data starts, and where the entry (with
    /// any data descriptor) ends.
    fn local(&self, c: &Central, cd_start: usize) -> Result<(usize, usize), String> {
        let at = c.offset as usize;
        if self.u32(at)? != LOCAL {
            return Err("its local header is missing".to_string());
        }
        let disagrees =
            |what: &str| format!("its local header disagrees with the central directory ({what})");
        if self.u16(at + 6)? != c.flags {
            return Err(disagrees("flags"));
        }
        if self.u16(at + 8)? != c.method {
            return Err(disagrees("compression method"));
        }
        let name_len = self.u16(at + 26)? as usize;
        let extra_len = self.u16(at + 28)? as usize;
        if self.bytes(at + 30, name_len)? != c.name {
            return Err(disagrees("name"));
        }
        let (crc, packed, size) = (self.u32(at + 14)?, self.u32(at + 18)?, self.u32(at + 22)?);
        let described = c.flags & FLAG_DESCRIPTOR != 0;
        let zeros = described && (crc, packed, size) == (0, 0, 0);
        if !zeros && (crc, packed, size) != (c.crc, c.packed, c.size) {
            return Err(disagrees("CRC or sizes"));
        }
        let start = at + 30 + name_len + extra_len;
        let mut end = start.saturating_add(c.packed as usize);
        if described {
            // An optional signature, then CRC and sizes. The CRC may equal the signature, so
            // both readings are tried.
            let says = |d: usize| -> bool {
                let at = |n: usize| self.u32(d.saturating_add(n));
                (at(0), at(4), at(8)) == (Ok(c.crc), Ok(c.packed), Ok(c.size))
            };
            end = match (
                self.u32(end) == Ok(DESCRIPTOR) && says(end.saturating_add(4)),
                says(end),
            ) {
                (true, _) => end.saturating_add(16),
                (false, true) => end.saturating_add(12),
                _ => {
                    return Err(
                        "its data descriptor disagrees with the central directory".to_string()
                    )
                }
            };
        }
        if end > cd_start {
            return Err("its data runs into the central directory".to_string());
        }
        Ok((start, end))
    }

    fn entry(&self, c: &Central, name: &str, start: usize) -> Result<Entry, String> {
        if c.flags & (FLAG_ENCRYPTED | FLAG_STRONG_ENCRYPTION | FLAG_MASKED_HEADER) != 0 {
            return Err("an encrypted entry".to_string());
        }
        match c.unix_type() {
            Some(0o120000) => {
                if c.size > MAX_LINK {
                    return Err("a link target longer than 4096 bytes".to_string());
                }
                let target = String::from_utf8(self.data(c, start)?)
                    .map_err(|_| "a link target that is not UTF-8".to_string())?;
                Ok(Entry::Symlink(target))
            }
            Some(0o010000) => Err("a FIFO".to_string()),
            Some(0o020000 | 0o060000) => Err("a device file".to_string()),
            Some(0o140000) => Err("a socket".to_string()),
            _ if name.ends_with('/') => match c.size {
                0 => Ok(Entry::Dir),
                _ => Err("a directory with data".to_string()),
            },
            Some(0o040000) => Err("a directory whose name doesn't end in `/`".to_string()),
            _ => Ok(Entry::File(self.data(c, start)?)),
        }
    }

    /// An entry's data, decompressed and checked against its size and CRC.
    fn data(&self, c: &Central, start: usize) -> Result<Vec<u8>, String> {
        let packed = self.bytes(start, c.packed as usize)?;
        let size = c.size as u64;
        let bytes = match c.method {
            0 if c.packed == c.size => packed.to_vec(),
            0 => return Err("a stored entry whose sizes differ".to_string()),
            8 => read_capped(DeflateDecoder::new(packed), size)
                .map_err(|e| format!("its deflated data is damaged: {e}"))?
                .ok_or("its data is larger than its recorded size")?,
            m => return Err(format!("compression method {m} (only stored and deflate)")),
        };
        if bytes.len() as u64 != size {
            return Err("its data is smaller than its recorded size".to_string());
        }
        let mut crc = Crc::new();
        crc.update(&bytes);
        if crc.sum() != c.crc {
            return Err("its CRC is wrong (a damaged archive?)".to_string());
        }
        Ok(bytes)
    }
}

/// Whether the extra field holds a zip64 record (header ID 1).
fn has_zip64(mut extra: &[u8]) -> bool {
    while extra.len() >= 4 {
        let id = u16::from_le_bytes([extra[0], extra[1]]);
        let len = u16::from_le_bytes([extra[2], extra[3]]) as usize;
        if id == 1 {
            return true;
        }
        extra = extra.get(4 + len..).unwrap_or(&[]);
    }
    false
}
