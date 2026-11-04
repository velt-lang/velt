//! std's parsed modules, kept in the build cache ([`StdAstCache`], veltc's `build_cache`) across builds: a std
//! file every program loads is parsed once per compiler, not once per `velt check`, `build`, `run`
//! or `test`.
//!
//! - An entry (kind [`KIND`]) is a module in `velt_syntax`'s compact form
//!   ([`velt_syntax::encode_module`]), decoded for the file id the source has in this build's
//!   source map, so spans come out as a parse would give them.
//! - The key is the compiler ([`StdAstCache::compiler`]: another or a rebuilt
//!   `velt` parses afresh), the encoding's format, whether the file is plain TypeScript, and the
//!   source's contents (its length, CRC-32 and a 64-bit hash). Nothing about where the file is:
//!   a `VELT_STD` pointing elsewhere or an edited std file in a source checkout is another key,
//!   never a stale entry.
//! - Only parses without diagnostics are stored (a file with errors is parsed every time, so its
//!   errors are reported every time), and only sources read from disk: the language server's
//!   unsaved buffers are parsed, never stored.
//! - An entry that fails the cache's check or does not decode is a miss: the file is parsed and
//!   the entry stored again. `VELT_BUILD_CACHE=off` turns this off with the rest of the cache
//!   (veltc then passes no [`StdAstCache`]).
//! - User code is never cached here: it can depend on the package's `define` constants.

use std::hash::BuildHasher;

use velt_build_cache::{Cache, Key, KeyHasher};
use velt_common::{Diagnostics, FileId};
use velt_syntax::ast;

/// The cache's kind for parsed std modules.
pub(crate) const KIND: &str = "std-ast";

/// The build cache a load keeps std's parses in ([`crate::LoadOptions::std_ast_cache`]).
#[derive(Clone, Copy)]
pub struct StdAstCache<'a> {
    /// The cache.
    pub cache: &'a Cache,
    /// Who compiles (veltc's `compiler_identity`): part of every key, so another or a rebuilt
    /// compiler parses afresh.
    pub compiler: Key,
}

/// How one load parses std files: through a cache, with the part of every key that is the same
/// for all files, or not.
#[derive(Default)]
pub(crate) struct StdParser<'a> {
    cache: Option<(&'a Cache, KeyHasher)>,
}

impl<'a> StdParser<'a> {
    pub(crate) fn new(cache: Option<StdAstCache<'a>>) -> Self {
        let cache = cache.map(|c| {
            let mut prefix = KeyHasher::new();
            prefix
                .part(KIND.as_bytes())
                .part(&c.compiler.0)
                .part(velt_syntax::AST_FORMAT);
            (c.cache, prefix)
        });
        StdParser { cache }
    }

    /// Parse std file `file` (`src`; `plain_ts`: a `.ts` file), or decode its cached parse.
    /// `cacheable`: `src` is the file on disk (not an editor's unsaved buffer).
    pub(crate) fn parse(
        &self,
        file: FileId,
        src: &str,
        plain_ts: bool,
        cacheable: bool,
    ) -> (ast::Module, Diagnostics) {
        match self.cache.as_ref().filter(|_| cacheable) {
            Some((cache, prefix)) => parse_cached(cache, prefix, file, src, plain_ts),
            None => parse_source(file, src, plain_ts),
        }
    }
}

/// The file id for parses whose spans nobody reads (the loader's look at a module's re-exports
/// and effects), and the one a module of file 0 is parsed under before it is stored: no source
/// map has this many files, and its spans cannot be mistaken for [`velt_common::Span::DUMMY`]'s.
pub(super) const NO_FILE: FileId = FileId(u32::MAX);

/// [`StdParser::parse`] with `cache`, keys starting with `prefix` (the compiler and the format).
pub(crate) fn parse_cached(
    cache: &Cache,
    prefix: &KeyHasher,
    file: FileId,
    src: &str,
    plain_ts: bool,
) -> (ast::Module, Diagnostics) {
    let key = key(prefix, src, plain_ts);
    let hit = cache
        .get_key(KIND, &key)
        .and_then(|bytes| velt_syntax::decode_module(&bytes, file));
    if let Some(module) = hit {
        return (module, Vec::new());
    }
    // File 0's empty spans at 0 are `Span::DUMMY`: parsed as file 0, a module would store them
    // as the dummy, which another build would not take for a span of its file.
    let parsed_as = if file == FileId(0) { NO_FILE } else { file };
    let (module, diags) = parse_source(parsed_as, src, plain_ts);
    if !diags.is_empty() {
        return match parsed_as == file {
            true => (module, diags),
            false => parse_source(file, src, plain_ts),
        };
    }
    let bytes = velt_syntax::encode_module(&module, parsed_as);
    cache.put_key(KIND, &key, &bytes);
    if parsed_as == file {
        return (module, diags);
    }
    match velt_syntax::decode_module(&bytes, file) {
        Some(module) => (module, diags),
        None => parse_source(file, src, plain_ts),
    }
}

fn parse_source(file: FileId, src: &str, plain_ts: bool) -> (ast::Module, Diagnostics) {
    if plain_ts {
        velt_syntax::parse_ts_file(file, src)
    } else {
        velt_syntax::parse_file(file, src)
    }
}

/// The key of `src`'s parse: `prefix` and the source's contents ([`contents`]).
pub(crate) fn key(prefix: &KeyHasher, src: &str, plain_ts: bool) -> Key {
    let mut h = prefix.clone();
    h.part(&contents(src, plain_ts));
    h.finish()
}

/// What a key holds of a source's contents: its length, CRC-32 and a 64-bit hash, and whether it
/// is plain TypeScript. The content hashes are cheap next to parsing (a CRC-32 in hardware and a
/// 64-bit multiply-fold hash), and with the length they make two different sources with one key
/// as unlikely as a collision of a 96-bit hash.
pub(crate) fn contents(src: &str, plain_ts: bool) -> [u8; 21] {
    let fold = foldhash::fast::FixedState::with_seed(0x05ee_da57).hash_one(src.as_bytes());
    let mut contents = [0u8; 21];
    contents[..8].copy_from_slice(&(src.len() as u64).to_le_bytes());
    contents[8..12].copy_from_slice(&crc32fast::hash(src.as_bytes()).to_le_bytes());
    contents[12..20].copy_from_slice(&fold.to_le_bytes());
    contents[20] = u8::from(plain_ts);
    contents
}

#[cfg(test)]
#[path = "std_ast_tests.rs"]
mod tests;
