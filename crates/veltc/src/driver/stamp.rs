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
//!   an import would now resolve to (a `.ts` beside a `.velt`, a `node_modules` package nearer
//!   the importer) is a change, and where each symbolic link to an npm package led; and the
//!   manifests and lockfile the package graph came from, with the workspace's file, member
//!   listings and lockfile when the package is in one;
//! - extra inputs of the link, by size and time;
//! - how many objects the build made, and at the end how many entries it has, so a stamp cut
//!   short is no stamp. It is written to a temporary file and renamed into place;
//! - the system libraries the program's C functions need: an unchanged build isn't checked, and
//!   the check is what finds them, but its link still needs them.
//!
//! The directories its paths are in are written once, in a table after the header (`d <dir>`),
//! and an entry names one as `~<n>/<name>`: a package of 2,000 files has 10,000 entries, and
//! with every path written whole most of a no-op build went to reading the stamp's text.
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
const HEADER: &str = "velt-build-stamp 6";

/// `<exe>.build-stamp`.
pub(crate) fn path(exe: &Path) -> PathBuf {
    let mut name = exe.file_name().unwrap_or_default().to_os_string();
    name.push(".build-stamp");
    exe.with_file_name(name)
}

/// Whether stamps are used: not with `$VELT_BUILD_STAMP=off`, except while a build needs one
/// whatever the environment says ([`required`]).
pub(crate) fn enabled() -> bool {
    REQUIRED.load(std::sync::atomic::Ordering::Relaxed) > 0
        || !std::env::var("VELT_BUILD_STAMP").is_ok_and(|v| v == "off" || v == "0")
}

static REQUIRED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Run `build` with stamps on: a source generator's host (`crate::generate`), whose stamp lists
/// the files the generators read and keeps an unchanged host from being compiled again.
pub(crate) fn required<T>(build: impl FnOnce() -> T) -> T {
    use std::sync::atomic::Ordering;
    REQUIRED.fetch_add(1, Ordering::Relaxed);
    let out = build();
    REQUIRED.fetch_sub(1, Ordering::Relaxed);
    out
}

/// The key of what a build is configured by (everything but the files it reads).
pub(super) fn options_key(opts: &BuildOptions, backend: Backend) -> Key {
    let mut h = KeyHasher::new();
    h.part(HEADER.as_bytes());
    h.part(&crate::build_cache::compiler_identity().0);
    // Its path too: two copies of one velt read std and the runtime beside each.
    h.part(format!("{:?}", std::env::current_exe().ok()).as_bytes());
    let BuildOptions {
        input,
        root_source,
        test_harness,
        packages: _, // From the manifest and lockfile, recorded with their directory.
        output,
        release,
        debug_info,
        target: _,
        host,
        cpu,
        emit,
        backend: _,
        report_numbers,
        debug_vars,
        no_generators: _, // A build with it is never stamped.
        check_variants,
    } = opts;
    h.part(format!("{:?}", vpm::relpath::absolute(input)).as_bytes());
    h.part(format!("{root_source:?} {test_harness}").as_bytes());
    let settings = format!(
        "{output:?} {release} {debug_info} {} {host:?} {cpu:?} {emit:?} {backend:?} \
         {report_numbers} {debug_vars} {check_variants} {}",
        opts.target(),
        opts.has_line_info()
    );
    h.part(settings.as_bytes());
    h.part(format!("{:?}", velt_env()).as_bytes());
    if backend == Backend::Llvm {
        let clang = velt_codegen_llvm::find_clang();
        let time = clang.as_deref().and_then(probe::stat);
        let version = velt_codegen_llvm::clang_version();
        h.part(format!("{clang:?} {time:?} {version:?}").as_bytes());
    }
    h.finish()
}

/// The `VELT_*` environment that can change what a build produces, sorted.
pub(crate) fn velt_env() -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = std::env::vars()
        .filter(|(k, _)| k.starts_with("VELT_") && !IGNORED_ENV.contains(&k.as_str()))
        .collect();
    env.sort();
    env
}

/// `VELT_*` variables that do not change what a build produces.
const IGNORED_ENV: &[&str] = &[
    "VELT_BUILD_CACHE",
    "VELT_BUILD_CACHE_SIZE",
    "VELT_CACHE_STATS",
    "VELT_BUILD_STAMP",
    "VELT_TEST_PLAN",
    // The number of threads: debug builds make the same object on any number.
    "VELT_JOBS",
];

/// Note (in [`vpm::probe`]) what the package graph of `opts` was made from: the directories
/// above the input looked in for a manifest, every package's manifest and lockfile, and the
/// workspace around the input.
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
    // The workspace around the input: whether there is one (a workspace file appearing above a
    // package changes its lock and its graph), its file, what its member globs list, its members'
    // manifests and its lockfile.
    if let Some(root) = vpm::workspace::find_root(&input) {
        let _ = vpm::workspace::Workspace::load(&root);
        let _ = probe::read_to_string(&root.join(vpm::lockfile::LOCK_FILE));
    }
}

/// Before a build that is no longer a no-op compiles anything: the fetched dependencies (git,
/// url) of `opts`'s package graph checked file by file against their locks. A no-op build only
/// looked at their directories (`vpm::tree_stamp`), and an edit in place must not reach a build
/// that compiles again.
pub(super) fn check_fetched(opts: &BuildOptions) -> Result<(), String> {
    opts.packages
        .as_ref()
        .map_or(Ok(()), vpm::PackageGraph::verify_fetched)
}

/// What a finished build read and made, besides its source map.
pub(super) struct Built<'a> {
    /// What module resolution looked at.
    pub seen: &'a Seen,
    /// A generated root (`velt test`'s harness), whose source is in the key.
    pub virtual_root: Option<&'a Path>,
    /// How many objects the build made.
    pub objects: usize,
    /// Files whose size and time must stay as they are: the objects the link reads and the
    /// executable it writes (outputs some other build may have rewritten), and later further
    /// link inputs.
    pub link_inputs: &'a [PathBuf],
    /// Objects of std functions from the std cache the link read (`std_cache`): the fast path
    /// links them again, so they too must keep their size and time (a cached file does while
    /// it is cached; rewritten or removed, the stamp no longer matches).
    pub std_objects: &'a [PathBuf],
    /// What the program reaches outside itself (`super::outside::reaches`: `-` for nothing).
    pub outside: &'a str,
    /// What the generators the build ran read: the `file`, `dir`, `probe` and `real` entries of
    /// their hosts' stamps.
    pub generator_inputs: &'a [String],
    /// The system libraries of the program's C functions (`Session::c_libs`), whose names hold no
    /// space (`vpm`'s manifest checks them).
    pub c_libs: &'a [String],
}

/// The stamp of a build configured by `key` that read the files in `sm` and `built`.
pub(super) fn render(key: &Key, sm: &SourceMap, built: &Built) -> String {
    let mut s = format!(
        "{HEADER}\nkey {}\nobjects {}\noutside {}\nc-libs {}\n",
        key.hex(),
        built.objects,
        built.outside,
        built.c_libs.join(" ")
    );
    let seen = built.seen;
    let mut files = seen.read.clone();
    for (_, file) in sm.files() {
        // Generated modules are no files: the generators' files stand for them.
        if Some(file.path.as_path()) == built.virtual_root
            || file.path.starts_with(crate::generate::GENERATED_DIR)
        {
            continue;
        }
        // Read other than through `probe` (none is): no time, so it is always hashed. A file
        // `probe` read is recorded with its hash already, and is not hashed again.
        files
            .entry(vpm::relpath::absolute(&file.path))
            .or_insert_with(|| probe::Read {
                stat: None,
                hash: probe::content_hash(file.src.as_bytes()),
            });
    }
    // What the build's generators read (entries of their hosts' stamps): files, and what
    // resolving their imports looked at (`dir`, `probe` and `real` entries, kept as they are).
    let mut generator_lookups = std::collections::BTreeSet::new();
    for line in built.generator_inputs {
        let Some((head, path)) = line.split_once('\t') else {
            continue;
        };
        if !head.starts_with("file ") {
            generator_lookups.insert(line.as_str());
            continue;
        }
        let mut fields = head.split(' ').skip(1);
        let Some(hash) = fields.next() else {
            continue;
        };
        let stat = (fields.next().and_then(|s| s.parse().ok()))
            .zip(fields.next().and_then(|t| t.parse().ok()));
        let read = probe::Read {
            stat,
            hash: hash.to_string(),
        };
        files.entry(PathBuf::from(path)).or_insert(read);
    }
    let mut lines: Vec<(String, String)> = vec![];
    for (path, read) in &files {
        let time = read
            .stat
            .map_or_else(String::new, |(size, t)| format!(" {size} {t}"));
        lines.push((
            format!("file {}{time}", read.hash),
            path.display().to_string(),
        ));
    }
    for (dir, hash) in &seen.listed {
        lines.push((format!("dir {hash}"), dir.display().to_string()));
    }
    for (path, kind) in &seen.probed {
        lines.push((
            format!("probe {}", kind.letter()),
            path.display().to_string(),
        ));
    }
    for (path, hash) in &seen.resolved {
        lines.push((format!("real {hash}"), path.display().to_string()));
    }
    for line in generator_lookups {
        if let Some((head, path)) = line.split_once('\t') {
            lines.push((head.to_string(), path.to_string()));
        }
    }
    for (kind, paths) in [("link", built.link_inputs), ("std", built.std_objects)] {
        for path in paths {
            let path = vpm::relpath::absolute(path);
            let time = probe::stat(&path).map_or_else(|| "-".into(), |(n, t)| format!("{n} {t}"));
            lines.push((format!("{kind} {time}"), path.display().to_string()));
        }
    }
    write_entries(&mut s, &lines);
    s
}

/// Append `lines` (head, path) to stamp text `s`: the directory table, the entries naming their
/// directory by its number, and `end`. A path or head holding a line break (`\n`, or `\r`,
/// which `str::lines` drops before one) would read as more lines than it is, and a `d ` line
/// in it as another directory of the table: then the stamp says only that, and never matches,
/// so such a build is never a no-op.
fn write_entries(s: &mut String, lines: &[(String, String)]) {
    let breaks = |t: &str| t.contains(['\n', '\r']);
    if (lines.iter()).any(|(head, path)| breaks(head) || breaks(path)) {
        s.push_str("unusable: a path holds a line break\n");
        return;
    }
    let mut dirs: Vec<&str> = vec![];
    let mut index: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    // Each line's head, its directory's number and name when the path has a directory, and the
    // path.
    type Named<'a> = (&'a str, Option<(usize, &'a str)>, &'a str);
    let mut named: Vec<Named> = Vec::with_capacity(lines.len());
    for (head, path) in lines {
        let split = path.rfind(std::path::is_separator).filter(|&i| i > 0);
        let short = split.map(|i| {
            let dir = &path[..i];
            let n = *index.entry(dir).or_insert_with(|| {
                dirs.push(dir);
                dirs.len() - 1
            });
            (n, &path[i + 1..])
        });
        named.push((head, short, path));
    }
    for dir in &dirs {
        let _ = writeln!(s, "d {dir}");
    }
    for (head, short, path) in &named {
        let _ = match short {
            Some((n, name)) => writeln!(s, "{head}\t~{n}/{name}"),
            None => writeln!(s, "{head}\t{path}"),
        };
    }
    let _ = writeln!(s, "end {}", lines.len());
}

/// The directory table of stamp text after its header lines (`lines`, advanced past it), and
/// the first entry line, if any.
fn dir_table<'a>(lines: &mut std::str::Lines<'a>) -> (Vec<&'a str>, Option<&'a str>) {
    let mut dirs = vec![];
    for line in lines.by_ref() {
        match line.strip_prefix("d ") {
            Some(dir) => dirs.push(dir),
            None => return (dirs, Some(line)),
        }
    }
    (dirs, None)
}

/// An entry's path, its directory number replaced by the directory.
fn expand<'a>(path: &'a str, dirs: &[&str]) -> Option<std::borrow::Cow<'a, str>> {
    let Some(rest) = path.strip_prefix('~') else {
        return Some(std::borrow::Cow::Borrowed(path));
    };
    let (n, name) = rest.split_once('/')?;
    let dir = dirs.get(n.parse::<usize>().ok()?)?;
    Some(std::borrow::Cow::Owned(format!(
        "{dir}{}{name}",
        std::path::MAIN_SEPARATOR
    )))
}

/// What the program `exe` reaches outside itself, as its build stamp records it
/// (`super::outside::reaches`; read a file's entry with `super::outside::of_file`), or `None`
/// without a stamp of this format.
pub(crate) fn outside(exe: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path(exe)).ok()?;
    let mut lines = text.lines();
    (lines.next()? == HEADER).then_some(())?;
    Some(lines.nth(2)?.strip_prefix("outside ")?.to_string())
}

/// The entries of stamp `text`: its lines between the header and `end`, with each path whole
/// (as [`compose`] takes them).
pub(crate) fn entry_lines(text: &str) -> Vec<String> {
    let mut lines = text.lines();
    let _header: Vec<&str> = lines.by_ref().take(5).collect();
    let (dirs, first) = dir_table(&mut lines);
    first
        .into_iter()
        .chain(lines)
        .take_while(|l| !l.starts_with("end "))
        .filter_map(|l| {
            let (head, path) = l.split_once('\t')?;
            Some(format!("{head}\t{}", expand(path, &dirs)?))
        })
        .collect()
}

/// A stamp of `key` with `objects` (a count its maker gives meaning to) and `entries` (lines as
/// [`render`] writes them), which [`still_valid`] checks like a build's.
pub(crate) fn compose(key: &Key, objects: usize, entries: &[String]) -> String {
    let mut s = format!(
        "{HEADER}\nkey {}\nobjects {objects}\noutside -\nc-libs \n",
        key.hex()
    );
    let lines: Vec<(String, String)> = (entries.iter())
        .filter_map(|l| l.split_once('\t'))
        .map(|(head, path)| (head.to_string(), path.to_string()))
        .collect();
    write_entries(&mut s, &lines);
    s
}

/// The entry of a directory listed with names hashing to `hash` ([`vpm::probe::Seen::listed`]).
pub(crate) fn dir_entry(dir: &Path, hash: &str) -> String {
    format!("dir {hash}\t{}", dir.display())
}

/// The `objects` of the stamp at `path` when it is of `key` and everything it records is
/// unchanged.
pub(crate) fn still_valid(path: &Path, key: &Key) -> Option<usize> {
    read(path, key).map(|(objects, _, _)| objects)
}

/// Write stamp `text` to `path` whole: into a temporary file renamed into place, so a build
/// stopped half-way leaves the old stamp or none, never part of one.
pub(crate) fn write(path: &Path, text: &str) -> std::io::Result<()> {
    let mut tmp = path.as_os_str().to_os_string();
    tmp.push(format!(".{}.tmp", std::process::id()));
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// The files the stamp at `path` records with a size and time older than the stamp itself (so
/// trusted by [`check`]), with their hashes: what [`probe::record_reusing`] may reuse for a file
/// that still has that size and time, instead of hashing it again.
pub(super) fn known(path: &Path) -> std::collections::BTreeMap<PathBuf, probe::Read> {
    let mut known = std::collections::BTreeMap::new();
    let (Ok(text), Some((_, written))) = (std::fs::read_to_string(path), probe::stat(path)) else {
        return known;
    };
    if text.lines().next() != Some(HEADER) {
        return known;
    }
    let mut lines = text.lines();
    let _header: Vec<&str> = lines.by_ref().take(5).collect();
    let (dirs, first) = dir_table(&mut lines);
    for line in first.into_iter().chain(lines) {
        let Some((head, file)) = line.split_once('\t') else {
            continue;
        };
        let Some(file) = expand(file, &dirs) else {
            continue;
        };
        let mut fields = head.split(' ');
        if fields.next() != Some("file") {
            continue;
        }
        let (Some(hash), Some(size), Some(time)) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let (Ok(size), Ok(time)) = (size.parse(), time.parse()) else {
            continue;
        };
        if time < written {
            let read = probe::Read {
                stat: Some((size, time)),
                hash: hash.to_string(),
            };
            known.insert(PathBuf::from(file.as_ref()), read);
        }
    }
    known
}

/// [`check`] of the stamp at `path`, against the time it was written: the number of unit
/// objects, the std cache's objects the link also needs, and the system libraries of its C
/// functions.
pub(super) fn read(path: &Path, key: &Key) -> Option<(usize, Vec<PathBuf>, Vec<String>)> {
    let text = std::fs::read_to_string(path).ok()?;
    let written = probe::stat(path).map(|(_, t)| t);
    check_all(&text, key, written)
}

/// Whether the stamp `text`, written at `written` (nanoseconds since 1970), still describes the
/// build configured by `key`: `Some(objects)` when everything it records is unchanged.
pub(crate) fn check(text: &str, key: &Key, written: Option<u128>) -> Option<usize> {
    check_all(text, key, written).map(|(objects, _, _)| objects)
}

/// [`check`], with what [`read`] needs too, in one pass over the text: the std cache's objects
/// (the `std` entries) and the system libraries of the program's C functions.
fn check_all(
    text: &str,
    key: &Key,
    written: Option<u128>,
) -> Option<(usize, Vec<PathBuf>, Vec<String>)> {
    let mut lines = text.lines();
    (lines.next()? == HEADER).then_some(())?;
    (lines.next()?.strip_prefix("key ")? == key.hex()).then_some(())?;
    let objects = lines.next()?.strip_prefix("objects ")?.parse().ok()?;
    lines.next()?.strip_prefix("outside ")?;
    let c_libs = lines.next()?.strip_prefix("c-libs ")?;
    let c_libs = c_libs.split_whitespace().map(String::from).collect();
    let (dirs, first) = dir_table(&mut lines);
    let mut std = vec![];
    let mut rest = first.into_iter().chain(lines.by_ref());
    // One buffer for every expanded path: this loop runs over every entry of every no-op build.
    // Entries come in runs from one directory, so its text stays in `buf` while the next entry
    // names the same number (`last`: that number, and where the name starts in `buf`).
    let mut buf = String::new();
    let mut last: Option<(&str, usize)> = None;
    for (entries, line) in rest.by_ref().enumerate() {
        if let Some(n) = line.strip_prefix("end ") {
            // Whole: as many entries as it says, and nothing after.
            (n.parse() == Ok(entries)).then_some(())?;
            return rest.next().is_none().then_some((objects, std, c_libs));
        }
        let (head, path) = line.split_once('\t')?;
        let path = match path.strip_prefix('~') {
            Some(short) => {
                let (n, name) = short.split_once('/')?;
                match last {
                    Some((at, len)) if at == n => buf.truncate(len),
                    _ => {
                        buf.clear();
                        buf.push_str(dirs.get(n.parse::<usize>().ok()?)?);
                        buf.push(std::path::MAIN_SEPARATOR);
                        last = Some((n, buf.len()));
                    }
                }
                buf.push_str(name);
                Path::new(buf.as_str())
            }
            None => Path::new(path),
        };
        let mut fields = head.split(' ');
        let kind = fields.next()?;
        let unchanged = match kind {
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
            "link" | "std" => {
                let rest: Vec<&str> = fields.collect();
                let now = probe::stat(path).map(|(n, t)| format!("{n} {t}"));
                now.as_deref().unwrap_or("-") == rest.join(" ")
            }
            _ => false,
        };
        unchanged.then_some(())?;
        if kind == "std" {
            std.push(path.to_path_buf());
        }
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
