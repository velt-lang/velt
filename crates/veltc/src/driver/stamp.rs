//! The build stamp: a build whose inputs did not change since the last build of the same output
//! is not compiled again (#1612 step 0). `velt run`, `velt test` and `velt build` run twice in a
//! row parse nothing the second time; the link step then checks its own stamp (`link.rs`).
//!
//! `<exe>.build-stamp` beside the executable records:
//! - a key over the compiler (its executable's size and time), the build options, a generated
//!   root's source, the `VELT_*` environment and, for LLVM builds, clang;
//! - every file the build read (all of the session's source map: std, packages, declaration
//!   files; and the configuration files module resolution read), by content hash and the size
//!   and modification time the file had when it was read: a file whose size or time differs is
//!   hashed again, so touching a file without changing it rebuilds nothing. As git does with
//!   "racily clean" files, a file whose time is not older than the stamp's own is hashed too: it
//!   may have changed within the time's granularity after it was read;
//! - what module resolution looked at ([`vpm::probe`]): every path it probed (still a file, a
//!   directory or missing as it was) and every directory it listed (the same entries), so a file
//!   an import would now resolve to (a `.ts` beside a `.vlt`, a `node_modules` package nearer
//!   the importer) is a change, and where each symbolic link to an npm package led; and the
//!   manifests and lockfile the package graph came from;
//! - extra inputs of the link, by size and time;
//! - how many objects the build made, and at the end how many entries it has, so a stamp cut
//!   short is no stamp. It is written to a temporary file and renamed into place.
//!
//! Only builds without diagnostics get a stamp, so a build that warned warns every time.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use velt_build_cache::{Key, KeyHasher};
use velt_common::SourceMap;
use vpm::probe::{self, Kind, Seen};

use super::BuildOptions;
use crate::backend::Backend;

/// The stamp's first line; a new format changes it, so old stamps just miss.
const HEADER: &str = "velt-build-stamp 2";

/// `<exe>.build-stamp`.
pub(super) fn path(exe: &Path) -> PathBuf {
    let mut name = exe.file_name().unwrap_or_default().to_os_string();
    name.push(".build-stamp");
    exe.with_file_name(name)
}

/// Whether stamps are used: not with `$VELT_BUILD_STAMP=off`.
pub(super) fn enabled() -> bool {
    !std::env::var("VELT_BUILD_STAMP").is_ok_and(|v| v == "off" || v == "0")
}

/// The key of what a build is configured by (everything but the files it reads).
pub(super) fn options_key(opts: &BuildOptions, backend: Backend) -> Key {
    let mut h = KeyHasher::new();
    h.part(HEADER.as_bytes());
    h.part(crate::commands::version::version_line().as_bytes());
    let exe = std::env::current_exe().ok();
    h.part(format!("{exe:?} {:?}", exe.as_deref().and_then(probe::stat)).as_bytes());
    let BuildOptions {
        input,
        root_source,
        packages: _, // From the manifest and lockfile, recorded with their directory.
        output,
        release,
        debug_info,
        target: _,
        cpu,
        emit,
        backend: _,
        report_numbers,
        debug_vars,
    } = opts;
    h.part(format!("{:?}", vpm::relpath::absolute(input)).as_bytes());
    h.part(format!("{root_source:?}").as_bytes());
    let settings = format!(
        "{output:?} {release} {debug_info} {} {cpu:?} {emit:?} {backend:?} {report_numbers} \
         {debug_vars} {}",
        opts.target(),
        opts.has_line_info()
    );
    h.part(settings.as_bytes());
    let mut env: Vec<(String, String)> = std::env::vars()
        .filter(|(k, _)| k.starts_with("VELT_") && !IGNORED_ENV.contains(&k.as_str()))
        .collect();
    env.sort();
    h.part(format!("{env:?}").as_bytes());
    if backend == Backend::Llvm {
        let clang = velt_codegen_llvm::find_clang();
        let time = clang.as_deref().and_then(probe::stat);
        h.part(format!("{clang:?} {time:?}").as_bytes());
    }
    h.finish()
}

/// `VELT_*` variables that do not change what a build produces.
const IGNORED_ENV: &[&str] = &[
    "VELT_BUILD_CACHE",
    "VELT_BUILD_CACHE_SIZE",
    "VELT_CACHE_STATS",
    "VELT_BUILD_STAMP",
    "VELT_TEST_PLAN",
];

/// Note (in [`vpm::probe`]) what the package graph of `opts` was made from: the directories
/// above the input looked in for a manifest, and every package's manifest and lockfile.
pub(super) fn note_project(opts: &BuildOptions) {
    use vpm::manifest::{LEGACY_MANIFEST_FILE, MANIFEST_FILE};
    let input = vpm::relpath::absolute(&opts.input);
    for dir in input.ancestors().skip(1) {
        if probe::is_file(&dir.join(MANIFEST_FILE))
            || probe::is_file(&dir.join(LEGACY_MANIFEST_FILE))
        {
            break;
        }
    }
    let roots = opts
        .packages
        .iter()
        .flat_map(|g| g.packages())
        .map(|p| &p.root);
    for root in roots {
        let files = [
            MANIFEST_FILE,
            vpm::lockfile::LOCK_FILE,
            vpm::lockfile::LEGACY_LOCK_FILE,
        ];
        for file in files {
            let _ = probe::read_to_string(&root.join(file));
        }
    }
}

/// What a finished build read and made, besides its source map.
pub(super) struct Built<'a> {
    /// What module resolution looked at.
    pub seen: &'a Seen,
    /// A generated root (`velt test`'s harness), whose source is in the key.
    pub virtual_root: Option<&'a Path>,
    /// How many objects the build made.
    pub objects: usize,
    /// Further files the link reads, whose size and time must stay as they are.
    pub link_inputs: &'a [PathBuf],
}

/// The stamp of a build configured by `key` that read the files in `sm` and `built`.
pub(super) fn render(key: &Key, sm: &SourceMap, built: &Built) -> String {
    let mut s = format!("{HEADER}\nkey {}\nobjects {}\n", key.hex(), built.objects);
    let seen = built.seen;
    let mut files = seen.read.clone();
    for (_, file) in sm.files() {
        if Some(file.path.as_path()) == built.virtual_root {
            continue;
        }
        // Read other than through `probe` (none is): no time, so it is always hashed.
        let read = probe::Read {
            stat: None,
            hash: probe::content_hash(file.src.as_bytes()),
        };
        files
            .entry(vpm::relpath::absolute(&file.path))
            .or_insert(read);
    }
    let mut entries = 0;
    for (path, read) in &files {
        let time = read
            .stat
            .map_or_else(String::new, |(size, t)| format!(" {size} {t}"));
        let _ = writeln!(s, "file {}{time}\t{}", read.hash, path.display());
        entries += 1;
    }
    for (dir, hash) in &seen.listed {
        let _ = writeln!(s, "dir {hash}\t{}", dir.display());
        entries += 1;
    }
    for (path, kind) in &seen.probed {
        let _ = writeln!(s, "probe {}\t{}", kind.letter(), path.display());
        entries += 1;
    }
    for (path, hash) in &seen.resolved {
        let _ = writeln!(s, "real {hash}\t{}", path.display());
        entries += 1;
    }
    for path in built.link_inputs {
        let path = vpm::relpath::absolute(path);
        let time = probe::stat(&path).map_or_else(|| "-".into(), |(n, t)| format!("{n} {t}"));
        let _ = writeln!(s, "link {time}\t{}", path.display());
        entries += 1;
    }
    let _ = writeln!(s, "end {entries}");
    s
}

/// Write stamp `text` to `path` whole: into a temporary file renamed into place, so a build
/// stopped half-way leaves the old stamp or none, never part of one.
pub(super) fn write(path: &Path, text: &str) -> std::io::Result<()> {
    let mut tmp = path.as_os_str().to_os_string();
    tmp.push(format!(".{}.tmp", std::process::id()));
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// [`check`] of the stamp at `path`, against the time it was written.
pub(super) fn read(path: &Path, key: &Key) -> Option<usize> {
    let text = std::fs::read_to_string(path).ok()?;
    let written = probe::stat(path).map(|(_, t)| t);
    check(&text, key, written)
}

/// Whether the stamp `text`, written at `written` (nanoseconds since 1970), still describes the
/// build configured by `key`: `Some(objects)` when everything it records is unchanged.
pub(super) fn check(text: &str, key: &Key, written: Option<u128>) -> Option<usize> {
    let mut lines = text.lines();
    (lines.next()? == HEADER).then_some(())?;
    (lines.next()?.strip_prefix("key ")? == key.hex()).then_some(())?;
    let objects = lines.next()?.strip_prefix("objects ")?.parse().ok()?;
    for (entries, line) in lines.by_ref().enumerate() {
        if let Some(n) = line.strip_prefix("end ") {
            // Whole: as many entries as it says, and nothing after.
            (n.parse() == Ok(entries)).then_some(())?;
            return lines.next().is_none().then_some(objects);
        }
        let (head, path) = line.split_once('\t')?;
        let path = Path::new(path);
        let mut fields = head.split(' ');
        let unchanged = match fields.next()? {
            "file" => {
                let hash = fields.next()?;
                let recorded = fields.next().zip(fields.next());
                file_unchanged(path, hash, recorded, written)
            }
            "dir" => probe::listing_hash(path).as_deref() == Some(fields.next()?),
            "probe" => {
                let mut letter = fields.next()?.chars();
                Some(Kind::of(path)) == letter.next().and_then(Kind::from_letter)
            }
            "real" => probe::resolved_hash(path) == fields.next()?,
            "link" => {
                let rest: Vec<&str> = fields.collect();
                let now = probe::stat(path).map(|(n, t)| format!("{n} {t}"));
                now.as_deref().unwrap_or("-") == rest.join(" ")
            }
            _ => false,
        };
        unchanged.then_some(())?;
    }
    None
}

/// Whether file `path` still has contents of hash `hash`: without hashing it when it has the
/// size and time `recorded` when it was read, and that time is older than the stamp's.
fn file_unchanged(
    path: &Path,
    hash: &str,
    recorded: Option<(&str, &str)>,
    written: Option<u128>,
) -> bool {
    let Some(now) = probe::stat(path) else {
        return false;
    };
    let same = recorded.is_some_and(|(size, time)| {
        (size.parse(), time.parse()) == (Ok(now.0), Ok(now.1)) && written.is_some_and(|w| now.1 < w)
    });
    same || std::fs::read(path).is_ok_and(|bytes| probe::content_hash(&bytes) == hash)
}

#[cfg(test)]
#[path = "stamp_tests.rs"]
mod tests;
