//! Prebuilt hook modules (velt_common `std_hooks/prebuilt.rs`; #1728, #2196): the objects of the
//! std modules whose `prebuilt` hooks take and return only numbers and strings, compiled once
//! with everything they reach (`prebuilt_object`) and stored in the build cache (kind
//! [`KIND`]). A build whose check meets a `hookCall` of such a hook asks [`Finder::linked`]:
//! where the object is there, sema checks the call as one of the declared function
//! `prebuilt::symbol` and never loads the module, lowering calls it as an extern, and the link
//! adds the object ([`place`]). Where it isn't, the build loads and compiles the module as it
//! would without this, so no output depends on the cache; with `VELT_STD_OBJECT_FILL` (`velt
//! cache warm`) or `VELT_PREBUILT_HOOKS=fill` it then compiles the object and stores it
//! ([`Finder::fill`]). `VELT_PREBUILT_HOOKS=0` turns prebuilt objects off.
//!
//! **Key.** Nothing about a program enters it: only numbers and strings cross, and the object is
//! compiled in a world of its own (a root that calls the hooks and nothing else). A lookup key
//! names the compiler (`build_cache::compiler_identity`), LLVM's optimization flag, the target,
//! the CPU (a debug build's: the target's default, as for the std cache's debug objects), the std
//! root and the module. Under it, a manifest (`<cache>/warm/hooks/<lookup key>`, rewritten by each
//! fill) lists every std file the object's world loaded, as build-stamp `file` entries (hash, size
//! and time), each entry point's VIR signature, and the object's own key, which also covers those
//! files' hashes. A manifest whose files changed (a size or time that differs, then a hash) finds
//! nothing. Its `file` entries go into the program's build stamp, so a changed std file rebuilds
//! a program that linked the object.
//!
//! **Which builds.** Executables built with the std cache on (`std_cache::wanted`, so not for
//! WebAssembly, whose programs load the module as before, nor `VELT_STD_OBJECT=0`), and `velt
//! check`, which looks the object up as a debug build would and links nothing. Not a source
//! generator host, nor `velt dev`'s JIT, which links nothing.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use velt_build_cache::{Key, KeyHasher};
use velt_common::std_hooks::{prebuilt, HOOKS};
use velt_vir::vir;

use super::Use;
use crate::driver::{BuildError, BuildOptions, OutputPaths, Session};

/// The build cache's kind for prebuilt hook modules' objects.
pub(super) const KIND: &str = "stdhook";
/// The first line of a manifest; a manifest of another form finds nothing.
const MANIFEST_HEADER: &str = "velt prebuilt hook module 1";

/// A module's object as a build found it.
#[derive(Clone, Debug)]
pub(crate) struct Found {
    /// The cached object.
    pub object: PathBuf,
    /// Per entry point: its symbol and its VIR signature (`entry` lines of the manifest).
    pub entries: Vec<String>,
    /// The manifest's `file` entries, for the program's build stamp.
    pub files: Vec<String>,
}

/// What a build looks up, and what it found.
pub(crate) struct Finder {
    cache: &'static velt_build_cache::Cache,
    /// The lookup key's parts every module shares.
    base: KeyHasher,
    std_root: PathBuf,
    /// The target and CPU objects are compiled for.
    target: String,
    cpu: Option<String>,
    /// Per module (its table entry): what was found, `None` for a miss.
    seen: Mutex<HashMap<&'static str, Option<Found>>>,
}

impl Finder {
    /// The finder of a build with `opts`, `None` where it uses no prebuilt objects (module docs).
    pub(crate) fn for_build(opts: &BuildOptions) -> Option<Finder> {
        // An executable as the std cache serves it; anything else is `velt check` (the driver
        // asks for no other), which looks up what a debug build would.
        let linking = opts.emit == crate::cli::Emit::Exe;
        let wanted = match linking {
            true => super::wanted(opts) != Use::Off,
            false => {
                let off = std::env::var("VELT_STD_OBJECT").is_ok_and(|v| v == "0");
                !off && !velt_codegen_llvm::is_wasm(&opts.target())
            }
        };
        if !wanted || prebuilt_off() {
            return None;
        }
        let target = opts.target();
        // A debug build links objects for the target's default CPU, where those run.
        let cpu = match opts.release && linking {
            true => opts.cpu.clone(),
            false if super::serve::default_cpu_runs(opts, &target) => None,
            false => return None,
        };
        let cache = crate::build_cache::cache()?;
        let std_root = velt_loader::std_root()?;
        let mut base = KeyHasher::new();
        base.part(MANIFEST_HEADER.as_bytes())
            .part(&crate::build_cache::compiler_identity().0)
            .part(velt_codegen_llvm::opt_flag().as_bytes())
            .part(target.as_bytes())
            .part(cpu.as_deref().unwrap_or("").as_bytes())
            .part(std_root.as_os_str().as_encoded_bytes());
        Some(Finder {
            cache,
            base,
            std_root,
            target,
            cpu,
            seen: Mutex::default(),
        })
    }

    /// Does the build link the object of hook `index`'s module (looked up once per module)?
    pub(crate) fn linked(&self, index: usize) -> bool {
        let h = &HOOKS[index];
        if !h.prebuilt {
            return false;
        }
        let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        let found = seen.entry(h.module_entry).or_insert_with(|| {
            let found = self.find(h.module_entry);
            if super::stats() {
                let what = if found.is_some() {
                    "object found"
                } else {
                    "no object"
                };
                eprintln!("velt: prebuilt hooks: {}: {what}", h.module());
            }
            found
        });
        found.is_some()
    }

    /// The lookup key of module `module_entry`.
    fn lookup_key(&self, module_entry: &str) -> Key {
        let mut h = self.base.clone();
        h.part(module_entry.as_bytes());
        h.finish()
    }

    fn manifest_path(&self, key: &Key) -> Option<PathBuf> {
        let root = crate::build_cache::root()?;
        Some(root.join("warm").join("hooks").join(key.hex()))
    }

    /// The object of module `module_entry` when the cache holds one for the std files as they
    /// are now.
    fn find(&self, module_entry: &str) -> Option<Found> {
        let key = self.lookup_key(module_entry);
        let text = std::fs::read_to_string(self.manifest_path(&key)?).ok()?;
        let mut manifest = Manifest::parse(&text)?;
        let mut restated = false;
        for f in &mut manifest.files {
            if let Some(fresh) = unchanged(f)? {
                *f = fresh;
                restated = true;
            }
        }
        // The same contents with another size and time (a checkout or a copy): recorded, so the
        // next build compares sizes and times again instead of hashing.
        if restated {
            if let Some(path) = self.manifest_path(&key) {
                let _ = write_atomically(&path, &manifest.render());
            }
        }
        let object_key = object_key(&key, &manifest.files);
        if object_key.hex() != manifest.object {
            return None;
        }
        let object = self.cache.path_of(KIND, &object_key)?;
        Some(Found {
            object,
            entries: manifest.entries,
            files: manifest.files,
        })
    }

    /// What the build found, per module (table entry).
    pub(crate) fn found(&self) -> Vec<(&'static str, Found)> {
        let seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        let mut found: Vec<_> = (seen.iter())
            .filter_map(|(m, f)| Some((*m, f.clone()?)))
            .collect();
        found.sort_by_key(|(m, _)| *m);
        found
    }

    /// `VELT_STD_OBJECT_FILL` (`velt cache warm`) or `VELT_PREBUILT_HOOKS=fill`, where clang is
    /// there: compile the objects of the modules this build looked for and missed, and store
    /// them. Each failure is said with `VELT_STD_OBJECT_STATS`; the build goes on either way.
    pub(crate) fn fill(&self) {
        let asked = std::env::var("VELT_PREBUILT_HOOKS").is_ok_and(|v| v == "fill");
        let clang = || velt_codegen_llvm::find_clang().is_some();
        if !(super::serve::fill() || asked && clang()) {
            return;
        }
        let missed: Vec<&'static str> = {
            let seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
            let mut missed: Vec<_> = (seen.iter())
                .filter(|(_, f)| f.is_none())
                .map(|(m, _)| *m)
                .collect();
            missed.sort_unstable();
            missed
        };
        for module_entry in missed {
            if let Err(why) = self.fill_one(module_entry) {
                if super::stats() {
                    eprintln!("velt: prebuilt hooks: no object of `{module_entry}`: {why}");
                }
            }
        }
    }

    fn fill_one(&self, module_entry: &str) -> Result<(), String> {
        let cpu = self.cpu.as_deref();
        let built = super::prebuilt_object::build(module_entry, &self.std_root, &self.target, cpu)?;
        let key = self.lookup_key(module_entry);
        let object_key = object_key(&key, &built.files);
        let stored = super::compile::store_kind(self.cache, KIND, &object_key, &built.object);
        if stored.is_none() {
            return Err("its object could not be stored".into());
        }
        let manifest = Manifest {
            object: object_key.hex(),
            entries: built.entries,
            files: built.files,
        };
        let path = self.manifest_path(&key).ok_or("no build cache")?;
        write_atomically(&path, &manifest.render()).map_err(|e| format!("{}: {e}", path.display()))
    }
}

/// `VELT_PREBUILT_HOOKS=0`: no prebuilt objects (every hook module loaded as before).
fn prebuilt_off() -> bool {
    std::env::var("VELT_PREBUILT_HOOKS").is_ok_and(|v| v == "0")
}

/// The object's key: the lookup key and the hashes of the files it was compiled from.
fn object_key(lookup: &Key, files: &[String]) -> Key {
    let mut h = KeyHasher::new();
    h.part(&lookup.0);
    for f in files {
        // The hash and the path, not the size and time: a copy of the same files is the same.
        if let Some((head, path)) = f.split_once('\t') {
            h.part(head.split(' ').nth(1).unwrap_or("").as_bytes());
            h.part(path.as_bytes());
        }
    }
    h.finish()
}

/// Is the file of stamp `file` entry `entry` as it records: `Some(None)` with the same size and
/// time, `Some(Some(entry))` (its entry now) with only the same hash, `None` when it changed.
fn unchanged(entry: &str) -> Option<Option<String>> {
    let (head, path) = entry.split_once('\t')?;
    let mut fields = head.split(' ').skip(1);
    let (hash, size, time) = (fields.next()?, fields.next(), fields.next());
    let path = Path::new(path);
    if let (Some((n, t)), Some(size), Some(time)) = (vpm::probe::stat(path), size, time) {
        if n.to_string() == size && t.to_string() == time {
            return Some(None);
        }
    }
    let src = std::fs::read_to_string(path).ok()?;
    (vpm::probe::content_hash(src.as_bytes()) == hash).then(|| Some(file_entry(path, &src)))
}

/// The build-stamp `file` entry of the std file at `path` holding `src`.
pub(super) fn file_entry(path: &Path, src: &str) -> String {
    let (size, time) = vpm::probe::stat(path).unwrap_or_default();
    let hash = vpm::probe::content_hash(src.as_bytes());
    format!("file {hash} {size} {time}\t{}", path.display())
}

/// What a manifest holds (module docs).
struct Manifest {
    object: String,
    entries: Vec<String>,
    files: Vec<String>,
}

impl Manifest {
    fn parse(text: &str) -> Option<Manifest> {
        let mut lines = text.lines();
        if lines.next()? != MANIFEST_HEADER {
            return None;
        }
        let object = lines.next()?.strip_prefix("object ")?.to_string();
        let mut entries = vec![];
        let mut files = vec![];
        for line in lines {
            if let Some(e) = line.strip_prefix("entry ") {
                entries.push(e.to_string());
            } else if line.starts_with("file ") {
                files.push(line.to_string());
            } else {
                return None;
            }
        }
        Some(Manifest {
            object,
            entries,
            files,
        })
    }

    fn render(&self) -> String {
        let mut s = format!("{MANIFEST_HEADER}\nobject {}\n", self.object);
        for e in &self.entries {
            s.push_str(&format!("entry {e}\n"));
        }
        for f in &self.files {
            s.push_str(f);
            s.push('\n');
        }
        s
    }
}

/// Write `text` to `path` through a temporary file beside it, so a reader never sees half of it.
fn write_atomically(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

/// An entry point's VIR signature as a manifest's `entry` line says it: `<symbol> <params> ->
/// <result>`.
pub(super) fn entry_line(symbol: &str, params: &[vir::Ty], ret: vir::Ty) -> String {
    format!("{symbol} {params:?} -> {ret:?}")
}

/// After lowering: the objects of the prebuilt modules `program` calls into, which `found` (what
/// the check found) has. Each extern of a prebuilt hook must have the signature its object's
/// entry point was compiled with; anything else is a compiler bug.
pub(crate) fn linked_objects(
    program: &vir::Program,
    found: &[(&'static str, Found)],
) -> Result<Vec<PathBuf>, String> {
    let mut objects = vec![];
    for (module_entry, f) in found {
        let mut called = false;
        for (_, h) in prebuilt::of_module(module_entry) {
            let symbol = prebuilt::symbol(h);
            let Some(e) = program.externs.iter().find(|e| e.symbol == symbol) else {
                continue;
            };
            let line = entry_line(&symbol, &e.params, e.ret);
            if !f.entries.contains(&line) {
                return Err(format!(
                    "ICE: the prebuilt hook `{}` is called as `{line}`, but its object defines {:?}",
                    h.name, f.entries
                ));
            }
            called = true;
        }
        if called {
            objects.push(f.object.clone());
        }
    }
    Ok(objects)
}

/// After a check with `finder`: fill what it missed (`VELT_STD_OBJECT_FILL`), and what it found,
/// whose std files go to the build stamp's inputs `stamp`.
pub(in crate::driver) fn found(
    finder: Finder,
    stamp: &mut Vec<String>,
) -> Vec<(&'static str, Found)> {
    finder.fill();
    let found = finder.found();
    for (_, f) in &found {
        stamp.extend(f.files.iter().cloned());
    }
    found
}

/// After `program` (for `target`) is optimized: the objects of the prebuilt modules it calls
/// into, placed for the link after the std cache's own (`hook<n>.<ext>`, linked or copied as
/// those are), in `sess.std_objects`.
pub(in crate::driver) fn link(
    sess: &mut Session,
    opts: &BuildOptions,
    target: &str,
    program: &vir::Program,
) -> Result<(), BuildError> {
    if sess.prebuilt.is_empty() {
        return Ok(());
    }
    let objects = linked_objects(program, &sess.prebuilt).map_err(BuildError::Ice)?;
    if objects.is_empty() {
        return Ok(());
    }
    let place =
        OutputPaths::new(&opts.input, opts.output.as_deref(), opts.emit, target).std_objects();
    if sess.turn.is_none() {
        sess.turn = place.lock(sess.verbose);
    }
    // No std object placed: what an earlier build left there goes.
    if sess.std_objects.is_empty() {
        place.clear();
    }
    for (n, from) in objects.iter().enumerate() {
        let placed = place.put_named(&format!("hook{n}"), from);
        sess.std_objects.push(placed.map_err(BuildError::Failed)?);
    }
    if super::stats() {
        eprintln!("velt: prebuilt hooks: {} objects linked", objects.len());
    }
    Ok(())
}
