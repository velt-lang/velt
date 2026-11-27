//! Prebuilt hook modules' objects that come with the toolchain (#3262): what `prebuilt`'s finder
//! looks at when the build cache has no object of a module.
//!
//! A cached object is found by a key that names this machine: the compiler's identity (its
//! executable's size and time), the absolute std root, and each std file's size and time. So an
//! object cannot be copied into a user's cache, and until `velt cache warm` has run (it needs
//! clang, and does not run in CI), or with the cache off, every build compiled the module into the
//! program. A release therefore carries the objects itself:
//!
//! ```text
//! <prefix>/lib/hooks/<target>/<module entry>.o
//! <prefix>/lib/hooks/<target>/<module entry>.manifest
//! ```
//!
//! for the target's default CPU (what every debug build links, and a release build without
//! `--cpu`). The manifest names nothing of a machine: the compiler's version line, the target,
//! the entry points and thread-local values as a cached manifest lists them, and each std file
//! the object's world loaded as its content hash and its path below the std root.
//!
//! **Found** ([`find`]) by an installed `velt` only (`<prefix>/bin/velt` with `<prefix>/std` as
//! its std root: a development build has no prefix), when the manifest's version line is this
//! compiler's, its target the build's, and every file's hash that of the std file as it is now.
//! The last is what keeps an object from being linked against std sources that were edited: it
//! reads and hashes each of those files. The finder then stores the object in the build cache
//! under this install's own key (`prebuilt::Finder`), so later builds find it there by sizes
//! and times alone; with the cache off, every build makes this check.
//!
//! **Made** ([`ship`]) when a release is packaged: `velt cache hooks --out <prefix>/lib/hooks`
//! compiles every prebuilt module's object as `velt cache warm` does and writes it into the
//! target's directory there.

use std::path::{Path, PathBuf};

use velt_common::std_hooks::HOOKS;

use super::prebuilt_object::Built;

/// The first line of a shipped manifest; one of another form finds nothing.
const HEADER: &str = "velt shipped hook module 2";
/// Its last line: a manifest cut short (a copy or an unpack that stopped) lacks it.
const END: &str = "end";

/// A shipped object that may be linked: the object, its bytes (as the manifest's hash says they
/// are), and what its manifest says, with its std files as build-stamp `file` entries of the
/// files as they are now.
#[derive(Debug)]
pub(super) struct Shipped {
    pub object: PathBuf,
    pub bytes: Vec<u8>,
    pub entries: Vec<String>,
    pub tls: Vec<String>,
    pub statics: Vec<String>,
    pub files: Vec<String>,
}

/// What a shipped manifest holds.
#[derive(Debug, PartialEq, Eq)]
struct Manifest {
    compiler: String,
    target: String,
    /// The object's content hash: a damaged object (cut short, emptied, replaced) is no match.
    object: String,
    entries: Vec<String>,
    tls: Vec<String>,
    statics: Vec<String>,
    /// Per std file: its content hash and its path below the std root, `/`-separated.
    files: Vec<(String, String)>,
}

impl Manifest {
    fn parse(text: &str) -> Option<Manifest> {
        // Whole: the end marker is its last line.
        let text = text.strip_suffix(&format!("\n{END}\n"))?;
        let mut lines = text.lines();
        if lines.next()? != HEADER {
            return None;
        }
        let compiler = lines.next()?.strip_prefix("compiler ")?.to_string();
        let target = lines.next()?.strip_prefix("target ")?.to_string();
        let object = lines.next()?.strip_prefix("object ")?.to_string();
        let (mut entries, mut tls, mut statics, mut files) = (vec![], vec![], vec![], vec![]);
        for line in lines {
            if let Some(e) = line.strip_prefix("entry ") {
                entries.push(e.to_string());
            } else if let Some(t) = line.strip_prefix("tls ") {
                velt_vir::tls::TlsCall::of_symbol(t)?;
                tls.push(t.to_string());
            } else if let Some(t) = line.strip_prefix("static ") {
                velt_common::std_hooks::prebuilt::of_static_symbol(t)?;
                statics.push(t.to_string());
            } else {
                let (hash, path) = line.strip_prefix("file ")?.split_once('\t')?;
                files.push((hash.to_string(), path.to_string()));
            }
        }
        // Every object's world loads std files: a manifest that lists none would vouch for the
        // object against nothing.
        if files.is_empty() {
            return None;
        }
        Some(Manifest {
            compiler,
            target,
            object,
            entries,
            tls,
            statics,
            files,
        })
    }

    fn render(&self) -> String {
        let mut s = format!(
            "{HEADER}\ncompiler {}\ntarget {}\nobject {}\n",
            self.compiler, self.target, self.object
        );
        for e in &self.entries {
            s.push_str(&format!("entry {e}\n"));
        }
        for t in &self.tls {
            s.push_str(&format!("tls {t}\n"));
        }
        for t in &self.statics {
            s.push_str(&format!("static {t}\n"));
        }
        for (hash, path) in &self.files {
            s.push_str(&format!("file {hash}\t{path}\n"));
        }
        s.push_str(END);
        s.push('\n');
        s
    }
}

/// Where an installed `velt` at `exe` with the std root `std_root` keeps the shipped objects for
/// `target`: `<prefix>/lib/hooks/<target>`, when `exe` is `<prefix>/bin/velt` and `std_root` is
/// `<prefix>/std`. `None` for anything else: a development build, a std root given by
/// `$VELT_STD`, a toolchain without the directory.
pub(super) fn dir_of(exe: &Path, std_root: &Path, target: &str) -> Option<PathBuf> {
    let bin = exe.parent()?;
    if bin.file_name()? != "bin" {
        return None;
    }
    let prefix = bin.parent()?;
    if prefix.join("std") != std_root {
        return None;
    }
    let dir = prefix.join("lib").join("hooks").join(target);
    dir.is_dir().then_some(dir)
}

/// The shipped object of module `module_entry` in `dir`, when it is this compiler's (version
/// line `compiler`), for `target`, and compiled from the std files under `std_root` as they are
/// now (module docs).
pub(super) fn find(
    dir: &Path,
    module_entry: &str,
    std_root: &Path,
    compiler: &str,
    target: &str,
) -> Option<Shipped> {
    let object = dir.join(format!("{module_entry}.o"));
    let text = std::fs::read_to_string(dir.join(format!("{module_entry}.manifest"))).ok()?;
    let manifest = Manifest::parse(&text)?;
    if manifest.compiler != compiler || manifest.target != target {
        return None;
    }
    // The object as it was written: anything else is not linked, nor stored in the cache.
    let bytes = std::fs::read(&object).ok()?;
    if vpm::probe::content_hash(&bytes) != manifest.object {
        return None;
    }
    let mut files = Vec::with_capacity(manifest.files.len());
    for (hash, rel) in &manifest.files {
        let path = below(std_root, rel)?;
        let src = std::fs::read_to_string(&path).ok()?;
        if &vpm::probe::content_hash(src.as_bytes()) != hash {
            return None;
        }
        files.push(super::prebuilt::file_entry(&path, &src));
    }
    Some(Shipped {
        object,
        bytes,
        entries: manifest.entries,
        tls: manifest.tls,
        statics: manifest.statics,
        files,
    })
}

/// The file `rel` (a manifest's `/`-separated path) below `std_root`: `None` for a path that is
/// not plainly below it (absolute, or with a `..`), which no manifest this compiler wrote has.
fn below(std_root: &Path, rel: &str) -> Option<PathBuf> {
    let mut path = std_root.to_path_buf();
    for part in rel.split('/') {
        if part.is_empty() || part == "." || part == ".." || part.contains(['\\', ':']) {
            return None;
        }
        path.push(part);
    }
    Some(path)
}

/// Write module `module_entry`'s object `built` (compiled from the std at `std_root` for
/// `target`) and its manifest into `dir`, as [`find`] reads them.
pub(super) fn write(
    dir: &Path,
    module_entry: &str,
    built: &Built,
    std_root: &Path,
    compiler: &str,
    target: &str,
) -> Result<(), String> {
    let mut files = vec![];
    for entry in &built.files {
        files.push(relative(entry, std_root)?);
    }
    let manifest = Manifest {
        compiler: compiler.to_string(),
        target: target.to_string(),
        object: vpm::probe::content_hash(&built.object),
        entries: built.entries.clone(),
        tls: built.tls.clone(),
        statics: built.statics.clone(),
        files,
    };
    let said = |path: &Path, e: std::io::Error| format!("{}: {e}", path.display());
    std::fs::create_dir_all(dir).map_err(|e| said(dir, e))?;
    let object = dir.join(format!("{module_entry}.o"));
    std::fs::write(&object, &built.object).map_err(|e| said(&object, e))?;
    let path = dir.join(format!("{module_entry}.manifest"));
    std::fs::write(&path, manifest.render()).map_err(|e| said(&path, e))
}

/// A build-stamp `file` entry (`file <hash> <size> <time>\t<path>`) as a shipped manifest's:
/// the hash and the path below `std_root`, `/`-separated. An error for a file outside it.
fn relative(entry: &str, std_root: &Path) -> Result<(String, String), String> {
    let bad = || format!("not a file entry: `{entry}`");
    let (head, path) = entry.split_once('\t').ok_or_else(bad)?;
    let hash = head.split(' ').nth(1).ok_or_else(bad)?;
    let rel = Path::new(path)
        .strip_prefix(std_root)
        .map_err(|_| format!("`{path}` is not below the std root {}", std_root.display()))?;
    let parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    Ok((hash.to_string(), parts.join("/")))
}

/// `velt cache hooks --out <hooks>`: compile the object of every module with a `prebuilt` hook,
/// from the std this `velt` finds, for `target`'s default CPU, and write each with its manifest
/// into `<hooks>/<target>`, as a toolchain carries them in `lib/hooks`. Per module: its name, and
/// why it failed if it did. The build cache is neither read nor written.
pub(crate) fn ship(hooks: &Path, target: &str) -> Vec<(String, Result<(), String>)> {
    let dir = &hooks.join(target);
    // Not the test module of prebuilt hooks (std/lang/hooks/prebuilt_probe.velt: no std code
    // calls it, so no program would link its object).
    let mut modules: Vec<&'static str> = (HOOKS.iter())
        .filter(|h| h.prebuilt && h.module_entry != "lang_hooks_prebuilt_probe")
        .map(|h| h.module_entry)
        .collect();
    modules.sort_unstable();
    modules.dedup();
    let compiler = crate::commands::version::version_line();
    let each = |m: &'static str| -> Result<(), String> {
        let std_root = velt_loader::std_root().ok_or("no std root")?;
        let built = super::prebuilt_object::build(m, &std_root, target, None)?;
        write(dir, m, &built, &std_root, &compiler, target)
    };
    modules
        .into_iter()
        .map(|m| (m.to_string(), each(m)))
        .collect()
}

#[cfg(test)]
#[path = "shipped_tests.rs"]
mod tests;
