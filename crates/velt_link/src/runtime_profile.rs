//! Which profile a runtime library was built with, read from the archive's symbol index (the
//! first member of a `!<arch>` archive: GNU `/`, BSD `__.SYMDEF`, MSVC's first linker member).
//! A debug `velt_rt` defines `VELT_RT_DEBUG_BUILD` (`velt_rt::build_profile`); only the index is
//! read, a few MB, not the whole archive (hundreds of MB for a debug runtime).

use std::fs::File;
use std::io::Read;
use std::path::Path;

/// The symbol only a debug runtime defines.
const DEBUG_MARKER: &[u8] = b"VELT_RT_DEBUG_BUILD";
/// Archive magic and member header size.
const MAGIC: &[u8] = b"!<arch>\n";
const HEADER: usize = 60;
/// Symbol indexes larger than this are not read (the debug runtime's is ~8 MB).
const MAX_INDEX: u64 = 256 << 20;

/// Whether the runtime library at `path` is a debug build; `None` if that can't be told (not an
/// archive, no symbol index, unreadable).
pub fn runtime_lib_is_debug(path: &Path) -> Option<bool> {
    let mut file = File::open(path).ok()?;
    let mut head = [0u8; MAGIC.len() + HEADER];
    file.read_exact(&mut head).ok()?;
    let index = read_first_member(&head, file)?;
    Some(contains(&index, DEBUG_MARKER))
}

/// The first member's bytes (for a BSD long name, `#1/<n>`, the name comes first; harmless here).
fn read_first_member(head: &[u8], rest: impl Read) -> Option<Vec<u8>> {
    let header = head.strip_prefix(MAGIC)?;
    if &header[58..60] != b"`\n" {
        return None;
    }
    let name = std::str::from_utf8(&header[..16]).ok()?.trim_end();
    let is_index = name == "/" || name == "/SYM64/" || name.starts_with("#1/");
    let size: u64 = std::str::from_utf8(&header[48..58])
        .ok()?
        .trim()
        .parse()
        .ok()?;
    if !is_index || size > MAX_INDEX {
        return None;
    }
    let mut bytes = Vec::with_capacity(size as usize);
    rest.take(size).read_to_end(&mut bytes).ok()?;
    Some(bytes)
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn archive(name: &str, index: &[u8]) -> Vec<u8> {
        let mut bytes = MAGIC.to_vec();
        let header = format!(
            "{name:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n",
            0,
            0,
            0,
            644,
            index.len()
        );
        bytes.extend_from_slice(header.as_bytes());
        bytes.extend_from_slice(index);
        bytes
    }

    fn check(bytes: &[u8]) -> Option<bool> {
        let dir = std::env::temp_dir().join(format!("velt_rt_profile_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("lib{}.a", bytes.len()));
        std::fs::write(&path, bytes).unwrap();
        let result = runtime_lib_is_debug(&path);
        std::fs::remove_file(&path).unwrap();
        result
    }

    #[test]
    fn reads_the_marker_from_gnu_and_bsd_symbol_indexes() {
        let debug = b"\0\0\0\x02_velt_rt_alloc\0_VELT_RT_DEBUG_BUILD\0";
        assert_eq!(check(&archive("/", debug)), Some(true));
        assert_eq!(check(&archive("#1/20", debug)), Some(true));
        assert_eq!(
            check(&archive("/", b"\0\0\0\x01velt_rt_alloc\0")),
            Some(false)
        );
    }

    #[test]
    fn unknown_without_a_symbol_index() {
        assert_eq!(check(&archive("velt_rt.o/", b"VELT_RT_DEBUG_BUILD")), None);
        assert_eq!(
            check(b"not an archive at all, just some text here....................."),
            None
        );
    }
}
