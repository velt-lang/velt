//! The build stamp: a build whose inputs did not change since the last build of the same output
//! is not compiled again (#1612 step 0). `velt run`, `velt test` and `velt build` run twice in a
//! row parse nothing the second time; the link step then checks its own stamp (`link.rs`).
//!
//! `<exe>.build-stamp` beside the executable records:
//! - a key over the compiler (its executable's size and time), the build options, a generated
//!   root's source, the `VELT_*` environment and, for LLVM builds, clang;
//! - every file the build read (all of the session's source map: std, packages, declaration
//!   files), by size, modification time and content hash: a file whose size or time changed is
//!   hashed again, so touching a file without changing it rebuilds nothing;
//! - the listing of every directory module resolution looked in (each read file's directory, and
//!   the directories above it up to its package's root), with the package and TypeScript
//!   configuration files and the lockfile in them: a new file that an import would now resolve
//!   to (a `.ts` beside a `.vlt`, an `index.ts`, a `node_modules` package) changes a listing;
//! - how many objects the build made.
//!
//! Only builds without diagnostics get a stamp, so a build that warned warns every time.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use velt_build_cache::{Key, KeyHasher};
use velt_common::SourceMap;

use super::BuildOptions;
use crate::backend::Backend;

/// The stamp's first line; a new format changes it, so old stamps just miss.
const HEADER: &str = "velt-build-stamp 1";

/// Files in a recorded directory whose contents change how imports resolve or what a program
/// is built with.
const CONFIG_FILES: &[&str] = &[
    "package.vlt",
    "package.json",
    "tsconfig.json",
    "jsconfig.json",
    "velt.lock.json",
];

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
    h.part(format!("{exe:?} {:?}", exe.as_deref().and_then(file_time)).as_bytes());
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
        let time = clang.as_deref().and_then(file_time);
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

/// The stamp of a build configured by `key` that read the files in `sm` (except `virtual_root`,
/// a generated root whose source is in the key) and made `objects` objects.
pub(super) fn render(
    key: &Key,
    sm: &SourceMap,
    virtual_root: Option<&Path>,
    objects: usize,
) -> String {
    let mut s = format!("{HEADER}\nkey {}\nobjects {objects}\n", key.hex());
    let mut dirs = BTreeSet::new();
    let mut tops = std::collections::HashMap::new();
    for (_, file) in sm.files() {
        if Some(file.path.as_path()) == virtual_root {
            continue;
        }
        let path = vpm::relpath::absolute(&file.path);
        // What the build read; if the disk has something else now, a hash that never matches.
        let hash = Key::of(&[file.src.as_bytes()]).hex();
        let time = file_time(&path).map_or_else(String::new, |(size, t)| format!("{size} {t}"));
        let _ = writeln!(s, "file {hash} {time}\t{}", path.display());
        let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
        let top = tops
            .entry(dir.clone())
            .or_insert_with(|| package_root(&dir))
            .clone();
        for d in dir.ancestors() {
            dirs.insert(d.to_path_buf());
            if Some(d) == top.as_deref() || top.is_none() {
                break;
            }
        }
    }
    for dir in dirs {
        let _ = writeln!(s, "dir {}\t{}", listing(&dir).hex(), dir.display());
    }
    s
}

/// Whether the stamp `text` still describes the build configured by `key`: `Some(objects)` when
/// every file and directory it records is unchanged.
pub(super) fn check(text: &str, key: &Key) -> Option<usize> {
    let mut lines = text.lines();
    (lines.next()? == HEADER).then_some(())?;
    (lines.next()?.strip_prefix("key ")? == key.hex()).then_some(())?;
    let objects = lines.next()?.strip_prefix("objects ")?.parse().ok()?;
    for line in lines {
        let (head, path) = line.split_once('\t')?;
        let path = Path::new(path);
        let mut fields = head.split(' ');
        match fields.next()? {
            "file" => {
                let hash = fields.next()?;
                let recorded = (fields.next(), fields.next());
                let now = file_time(path)?;
                let same_time = recorded
                    == (
                        Some(now.0.to_string().as_str()),
                        Some(now.1.to_string().as_str()),
                    );
                if !same_time {
                    let bytes = std::fs::read(path).ok()?;
                    (Key::of(&[&bytes]).hex() == hash).then_some(())?;
                }
            }
            "dir" => (listing(path).hex() == fields.next()?).then_some(())?,
            _ => return None,
        }
    }
    Some(objects)
}

/// The nearest directory at or above `dir` with a package manifest (`package.vlt` or
/// `package.json`): where module resolution stops looking for packages of the files below.
fn package_root(dir: &Path) -> Option<PathBuf> {
    dir.ancestors()
        .find(|d| {
            d.join(vpm::manifest::MANIFEST_FILE).is_file() || d.join("package.json").is_file()
        })
        .map(Path::to_path_buf)
}

/// The hash of `dir`'s entries (names and kinds) and of the configuration files among them.
fn listing(dir: &Path) -> Key {
    let mut h = KeyHasher::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        h.part(b"missing");
        return h.finish();
    };
    let mut names: Vec<(String, bool)> = entries
        .flatten()
        .map(|e| {
            let is_dir = e.file_type().is_ok_and(|t| t.is_dir());
            (e.file_name().to_string_lossy().into_owned(), is_dir)
        })
        .collect();
    names.sort();
    for (name, is_dir) in &names {
        h.part(name.as_bytes()).part(&[u8::from(*is_dir)]);
        if CONFIG_FILES.contains(&name.as_str()) {
            h.part(&std::fs::read(dir.join(name)).unwrap_or_default());
        }
    }
    h.finish()
}

/// Size and modification time (nanoseconds since 1970) of `path`.
fn file_time(path: &Path) -> Option<(u64, u128)> {
    let meta = std::fs::metadata(path).ok()?;
    let time = meta.modified().ok()?;
    let nanos = time.duration_since(SystemTime::UNIX_EPOCH).ok()?.as_nanos();
    Some((meta.len(), nanos))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(dir: &Path, sm: &mut SourceMap, files: &[(&str, &str)]) {
        for (name, src) in files {
            let path = dir.join(name);
            std::fs::write(&path, src).unwrap();
            sm.add(path, *src);
        }
    }

    #[test]
    fn unchanged_inputs_match() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("package.vlt"), "{}").unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir(&src).unwrap();
        let mut sm = SourceMap::new();
        build(&src, &mut sm, &[("main.vlt", "a"), ("lib.vlt", "b")]);
        let key = Key::of(&[b"opts"]);
        let stamp = render(&key, &sm, None, 2);
        assert_eq!(check(&stamp, &key), Some(2));
        assert_eq!(check(&stamp, &Key::of(&[b"other"])), None, "other options");

        // Same contents, new time: still unchanged.
        std::fs::write(src.join("lib.vlt"), "b").unwrap();
        assert_eq!(check(&stamp, &key), Some(2));
        // Another content: changed.
        std::fs::write(src.join("lib.vlt"), "c").unwrap();
        assert_eq!(check(&stamp, &key), None);
        std::fs::write(src.join("lib.vlt"), "b").unwrap();
        assert_eq!(check(&stamp, &key), Some(2));

        // A new file where imports look (beside a module, or a package above it): changed.
        std::fs::write(src.join("lib.ts"), "").unwrap();
        assert_eq!(check(&stamp, &key), None);
        std::fs::remove_file(src.join("lib.ts")).unwrap();
        assert_eq!(check(&stamp, &key), Some(2));
        std::fs::create_dir(tmp.path().join("node_modules")).unwrap();
        assert_eq!(check(&stamp, &key), None);
        std::fs::remove_dir(tmp.path().join("node_modules")).unwrap();
        // The manifest's contents count.
        std::fs::write(tmp.path().join("package.vlt"), "{ }").unwrap();
        assert_eq!(check(&stamp, &key), None);
    }

    #[test]
    fn a_deleted_file_or_a_broken_stamp_misses() {
        let tmp = tempfile::tempdir().unwrap();
        let mut sm = SourceMap::new();
        build(tmp.path(), &mut sm, &[("main.vlt", "a")]);
        let key = Key::of(&[b"opts"]);
        let stamp = render(&key, &sm, None, 1);
        assert_eq!(check(&stamp[..stamp.len() / 2], &key), None);
        assert_eq!(check("", &key), None);
        std::fs::remove_file(tmp.path().join("main.vlt")).unwrap();
        assert_eq!(check(&stamp, &key), None);
    }

    #[test]
    fn a_generated_root_is_left_to_the_key() {
        let tmp = tempfile::tempdir().unwrap();
        let mut sm = SourceMap::new();
        let root = tmp.path().join("harness.vlt");
        sm.add(&root, "generated");
        let key = Key::of(&[b"opts"]);
        let stamp = render(&key, &sm, Some(&root), 1);
        assert!(!stamp.contains("harness.vlt"));
        assert_eq!(check(&stamp, &key), Some(1));
    }
}
