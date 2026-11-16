//! Which profile a runtime library was built with, read from the archive's symbol index (the
//! first member of a `!<arch>` archive: GNU `/`, BSD `__.SYMDEF`, MSVC's first linker member).
//! A debug `velt_rt` defines `VELT_RT_DEBUG_BUILD` (`velt_rt::build_profile`); only the index is
//! read, a few MB, not the whole archive (hundreds of MB for a debug runtime). The same index
//! tells whether the runtime has mimalloc's functions, which release builds call directly
//! (std/lang/hooks/heap.velt): a runtime built without the `mimalloc` feature doesn't.

use std::fs::File;
use std::io::Read;
use std::path::Path;

/// The symbol only a debug runtime defines.
const DEBUG_MARKER: &[u8] = b"VELT_RT_DEBUG_BUILD";
/// mimalloc's functions std's heap hooks call (std/lang/hooks/heap.velt).
const MIMALLOC: &[&[u8]] = &[
    b"mi_malloc",
    b"mi_malloc_aligned",
    b"mi_realloc",
    b"mi_realloc_aligned",
    b"mi_free",
];
/// Archive magic and member header size.
const MAGIC: &[u8] = b"!<arch>\n";
const HEADER: usize = 60;
/// Symbol indexes larger than this are not read (the debug runtime's is ~8 MB).
const MAX_INDEX: u64 = 256 << 20;

/// Whether the runtime library at `path` is a debug build; `None` if that can't be told (not an
/// archive, no symbol index, unreadable).
pub fn runtime_lib_is_debug(path: &Path) -> Option<bool> {
    runtime_lib_profile(path).map(|p| p.debug)
}

/// What a runtime library's symbol index says about it ([`runtime_lib_profile`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuntimeProfile {
    /// A debug build: its allocator checks and counts blocks (`VELT_RT_DEBUG_ALLOC`,
    /// `VELT_RC_STATS`) when the program runs.
    pub debug: bool,
    /// It defines mimalloc's `mi_malloc`, `mi_malloc_aligned`, `mi_realloc`,
    /// `mi_realloc_aligned` and `mi_free` (the `mimalloc` feature, on by default).
    pub mimalloc: bool,
}

/// The profile of the runtime library at `path`; `None` if that can't be told (not an archive,
/// no symbol index, unreadable).
pub fn runtime_lib_profile(path: &Path) -> Option<RuntimeProfile> {
    let mut file = File::open(path).ok()?;
    let mut head = [0u8; MAGIC.len() + HEADER];
    file.read_exact(&mut head).ok()?;
    let index = read_first_member(&head, file)?;
    Some(RuntimeProfile {
        debug: contains(&index, DEBUG_MARKER),
        mimalloc: MIMALLOC.iter().all(|name| defines(&index, name)),
    })
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

/// Whether the index names the C symbol `name` itself: a whole NUL-terminated name, with the
/// Mach-O `_` prefix or none (`mi_free`, not `_mi_free_generic` nor `mi_free_size`).
fn defines(index: &[u8], name: &[u8]) -> bool {
    let ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'$' || b == b'.';
    let mut at = 0;
    while let Some(i) = index[at..]
        .windows(name.len() + 1)
        .position(|w| &w[..name.len()] == name && w[name.len()] == 0)
    {
        let start = at + i;
        let whole = match &index[..start] {
            [] | [b'_'] => true,
            [.., b] if !ident(*b) => true,
            [.., b, b'_'] => !ident(*b),
            _ => false,
        };
        if whole {
            return true;
        }
        at = start + 1;
    }
    false
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

    fn profile(index: &[u8]) -> Option<RuntimeProfile> {
        let bytes = archive("/", index);
        let dir = std::env::temp_dir().join(format!("velt_rt_profile_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("mi{}.a", bytes.len()));
        std::fs::write(&path, bytes).unwrap();
        let result = runtime_lib_profile(&path);
        std::fs::remove_file(&path).unwrap();
        result
    }

    #[test]
    fn mimalloc_needs_each_function_by_its_whole_name() {
        // A GNU index: the count, then each name NUL-terminated.
        let index = |names: &[&str]| {
            let mut bytes = (names.len() as u32).to_be_bytes().to_vec();
            for name in names {
                bytes.extend_from_slice(name.as_bytes());
                bytes.push(0);
            }
            bytes
        };
        let five = [
            "mi_malloc",
            "mi_malloc_aligned",
            "mi_realloc",
            "mi_realloc_aligned",
            "mi_free",
        ];
        let p = profile(&index(&five)).unwrap();
        assert!(p.mimalloc && !p.debug);
        // Mach-O names, with the `_` prefix, and a debug runtime.
        let mut macho: Vec<String> = five.iter().map(|n| format!("_{n}")).collect();
        macho.push("_VELT_RT_DEBUG_BUILD".into());
        let macho: Vec<&str> = macho.iter().map(String::as_str).collect();
        let expected = RuntimeProfile {
            debug: true,
            mimalloc: true,
        };
        assert_eq!(profile(&index(&macho)), Some(expected));
        // Without the `mimalloc` feature: only names that contain mimalloc's.
        let none = [
            "__mi_malloc_generic",
            "_mi_malloc_aligned_at",
            "mi_malloc_size",
            "xmi_realloc",
            "mi_realloc_aligned2",
            "mi_freed",
        ];
        assert!(!profile(&index(&none)).unwrap().mimalloc);
        // One of the five missing.
        assert!(!profile(&index(&five[..4])).unwrap().mimalloc);
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
