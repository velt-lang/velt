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
//! **Thread-local values.** The object may keep state of its module's own (`prebuilt.rs`
//! "State"): its thread-local values are the program's to define, as with any object compiled to
//! link into a program (vir.rs invariant 5), so the manifest lists their externs (`tls` lines) and
//! a build that links the object adds them to its program ([`link`]), whose unit defining
//! `velt_main` defines their storage. Where a debug build's backend has no thread-local storage
//! an object shares (`velt_codegen_cl::has_thread_local_data`), an object with any is a miss.
//!
//! **Pinned.** Once sema has called the object's entry points, the build has to link it: a build
//! that links takes the object found out of the cache's reach at once ([`pin`]: a hard link or a
//! copy in a temporary directory the build holds), so another build's garbage collection or
//! `velt clean --cache` between the check and the link cannot fail the build. An object that
//! cannot be pinned is a miss, and the build loads the module.
//!
//! **Which builds.** Executables built with the std cache on (`std_cache::wanted`, so not for
//! WebAssembly, whose programs load the module as before, nor `VELT_STD_OBJECT=0`), and `velt
//! check`, which looks the object up as a debug build would and links nothing. Not a source
//! generator host, nor `velt dev`'s JIT, which links nothing.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use velt_build_cache::{Key, KeyHasher};
use velt_common::std_hooks::{prebuilt, HOOKS};
use velt_vir::vir;

use super::Use;
use crate::driver::{BuildError, BuildOptions, OutputPaths, Session};

/// The build cache's kind for prebuilt hook modules' objects.
pub(super) const KIND: &str = "stdhook";
/// The first line of a manifest; a manifest of another form finds nothing.
const MANIFEST_HEADER: &str = "velt prebuilt hook module 2";

/// A module's object as a build found it.
#[derive(Clone, Debug)]
pub(crate) struct Found {
    /// The object: pinned for a build that links ([`pin`]), else the cache's file.
    pub object: PathBuf,
    /// Per entry point: its symbol and its VIR signature (`entry` lines of the manifest).
    pub entries: Vec<String>,
    /// The manifest's `file` entries, for the program's build stamp.
    pub files: Vec<String>,
    /// The externs of its thread-local values (`tls` lines), which the program defines.
    pub tls: Vec<String>,
    /// The temporary directory holding the pinned object, removed when the last copy goes.
    pub _pinned: Option<Arc<Pinned>>,
}

/// A temporary directory of a build's own holding a pinned object ([`pin`]), removed when it is
/// dropped.
#[derive(Debug)]
pub(crate) struct Pinned(PathBuf);

impl Drop for Pinned {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// What a build looks up, and what it found.
pub(crate) struct Finder {
    cache: &'static velt_build_cache::Cache,
    /// Where the manifests are: `<cache>/warm/hooks`.
    manifests: PathBuf,
    /// The lookup key's parts every module shares.
    base: KeyHasher,
    std_root: PathBuf,
    /// The target and CPU objects are compiled for.
    target: String,
    cpu: Option<String>,
    /// The build links what it finds: each object is pinned as it is found ([`pin`]).
    linking: bool,
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
        let manifests = crate::build_cache::root()?.join("warm").join("hooks");
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
            manifests,
            base,
            std_root,
            target,
            cpu,
            linking,
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

    fn manifest_path(&self, key: &Key) -> PathBuf {
        self.manifests.join(key.hex())
    }

    /// The object of module `module_entry` when the cache holds one for the std files as they
    /// are now, pinned where the build links.
    fn find(&self, module_entry: &str) -> Option<Found> {
        let key = self.lookup_key(module_entry);
        let text = std::fs::read_to_string(self.manifest_path(&key)).ok()?;
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
            let _ = write_atomically(&self.manifest_path(&key), &manifest.render());
        }
        let object_key = object_key(&key, &manifest.files);
        if object_key.hex() != manifest.object {
            return None;
        }
        if !manifest.tls.is_empty() && !velt_codegen_cl::has_thread_local_data(&self.target) {
            return None;
        }
        let cached = self.cache.path_of(KIND, &object_key)?;
        let (object, pinned) = match self.linking {
            true => pin(&cached).map(|(dir, object)| (object, Some(Arc::new(dir))))?,
            false => (cached, None),
        };
        Some(Found {
            object,
            entries: manifest.entries,
            files: manifest.files,
            tls: manifest.tls,
            _pinned: pinned,
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
            tls: built.tls,
        };
        let path = self.manifest_path(&key);
        write_atomically(&path, &manifest.render()).map_err(|e| format!("{}: {e}", path.display()))
    }
}

/// The cached object `cached` taken out of the cache's reach for this build: a hard link to it
/// (a copy where the file system has none) in a temporary directory of the build's own, removed
/// with the directory. `None` when it is gone already.
fn pin(cached: &Path) -> Option<(Pinned, PathBuf)> {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    static SWEPT: std::sync::Once = std::sync::Once::new();
    let temp = std::env::temp_dir();
    SWEPT.call_once(|| sweep_stale_pins(&temp, std::time::SystemTime::now()));
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = temp.join(format!("{PIN_PREFIX}{}-{n}", std::process::id()));
    // A new directory only this user can write to: one that is there already (left by a killed
    // process of the same id, or made by someone else) is a miss.
    #[cfg(unix)]
    let created =
        std::os::unix::fs::DirBuilderExt::mode(&mut std::fs::DirBuilder::new(), 0o700).create(&dir);
    #[cfg(not(unix))]
    let created = std::fs::DirBuilder::new().create(&dir);
    created.ok()?;
    let dir = Pinned(dir);
    let to = dir.0.join("object");
    let placed = std::fs::hard_link(cached, &to).or_else(|_| std::fs::copy(cached, &to).map(drop));
    placed.ok().map(|()| (dir, to))
}

/// What the name of every pin directory ([`pin`]) starts with: `velt-hook-<pid>-<n>`.
const PIN_PREFIX: &str = "velt-hook-";

/// How old a pin directory has to be to be taken as left by a build that was killed: older than
/// any build runs.
const STALE_PIN: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

/// Remove the pin directories in `temp` last changed more than [`STALE_PIN`] before `now`: a build
/// killed between its check and its end (`kill -9`, Ctrl-C) leaves its own, holding a hard link
/// to an object (which keeps the cache's file alive) or a copy of one. Once per process, at its
/// first pin. A directory it can't remove (another user's) stays.
fn sweep_stale_pins(temp: &Path, now: std::time::SystemTime) {
    let Ok(entries) = std::fs::read_dir(temp) else {
        return;
    };
    for e in entries.flatten() {
        if !e.file_name().to_string_lossy().starts_with(PIN_PREFIX) {
            continue;
        }
        let changed = e.metadata().and_then(|m| m.modified());
        let stale = changed.is_ok_and(|t| now.duration_since(t).is_ok_and(|age| age > STALE_PIN));
        if stale && e.file_type().is_ok_and(|t| t.is_dir()) {
            let _ = std::fs::remove_dir_all(e.path());
        }
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
    tls: Vec<String>,
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
        let mut tls = vec![];
        for line in lines {
            if let Some(e) = line.strip_prefix("entry ") {
                entries.push(e.to_string());
            } else if let Some(t) = line.strip_prefix("tls ") {
                velt_vir::tls::TlsCall::of_symbol(t)?;
                tls.push(t.to_string());
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
            tls,
        })
    }

    fn render(&self) -> String {
        let mut s = format!("{MANIFEST_HEADER}\nobject {}\n", self.object);
        for e in &self.entries {
            s.push_str(&format!("entry {e}\n"));
        }
        for t in &self.tls {
            s.push_str(&format!("tls {t}\n"));
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
) -> Result<(Vec<PathBuf>, Vec<String>), String> {
    let mut objects = vec![];
    let mut tls = vec![];
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
            tls.extend(f.tls.iter().cloned());
        }
    }
    Ok((objects, tls))
}

/// Add the externs `tls` (the thread-local values of the objects `program` links) to `program`
/// where it lacks them: the unit defining `velt_main` defines the storage of each (velt_codegen_llvm
/// `function/tls.rs`, velt_codegen_cl `tls.rs`).
fn define_tls(program: &mut vir::Program, tls: &[String]) {
    for symbol in tls {
        let Some(call) = velt_vir::tls::TlsCall::of_symbol(symbol) else {
            continue;
        };
        if program.externs.iter().any(|e| &e.symbol == symbol) {
            continue;
        }
        let (params, ret) = call.signature();
        program.externs.push(vir::ExternFn {
            symbol: symbol.clone(),
            params,
            ret,
            noreturn: false,
            host: None,
            c_decl: None,
            variadic: false,
            c_abi: None,
        });
    }
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
/// those are), in `sess.std_objects`, and their thread-local values' externs added to `program`,
/// which defines their storage (module docs).
pub(in crate::driver) fn link(
    sess: &mut Session,
    opts: &BuildOptions,
    target: &str,
    program: &mut vir::Program,
) -> Result<(), BuildError> {
    if sess.prebuilt.is_empty() {
        return Ok(());
    }
    let (objects, tls) = linked_objects(program, &sess.prebuilt).map_err(BuildError::Ice)?;
    if objects.is_empty() {
        return Ok(());
    }
    define_tls(program, &tls);
    let place =
        OutputPaths::new(&opts.input, opts.output.as_deref(), opts.emit, target).std_objects();
    if sess.turn.is_none() {
        sess.turn = place.lock(sess.verbose);
    }
    // No std object placed: what an earlier build left there goes.
    if sess.std_objects.is_empty() {
        place.clear();
    }
    let placed = put(&place, &objects).map_err(BuildError::Failed)?;
    sess.std_objects.extend(placed);
    if super::stats() {
        eprintln!("velt: prebuilt hooks: {} objects linked", objects.len());
    }
    Ok(())
}

/// Place `objects` (pinned when found) in `place` for the link, as `hook<n>`. An `Err` is this
/// build's own output directory failing, not the cache.
fn put(place: &super::Place, objects: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    (objects.iter().enumerate())
        .map(|(n, from)| place.put_named(&format!("hook{n}"), from))
        .collect()
}

/// `velt cache warm`: compile and store the object of every module with a `prebuilt` hook that
/// the cache lacks, for a build like one with `opts` (`release` and `cpu` are what count), so a
/// hook no warm-up program calls is served too. Per module: its name, and whether it was stored
/// now (`Ok(false)`: there already) or why not.
pub(crate) fn warm(opts: &BuildOptions) -> Vec<(String, Result<bool, String>)> {
    let Some(mut finder) = Finder::for_build(opts) else {
        return vec![];
    };
    // It links nothing: nothing to pin.
    finder.linking = false;
    let mut modules: Vec<&'static str> = (HOOKS.iter())
        .filter(|h| h.prebuilt)
        .map(|h| h.module_entry)
        .collect();
    modules.sort_unstable();
    modules.dedup();
    let each = |m: &'static str| match finder.find(m) {
        Some(_) => Ok(false),
        None => finder.fill_one(m).map(|()| true),
    };
    modules
        .into_iter()
        .map(|m| (m.to_string(), each(m)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A finder over the cache in `dir` that links what it finds.
    fn finder(dir: &Path) -> Finder {
        let cache = velt_build_cache::Cache::new(&dir.join("cache"), u64::MAX);
        Finder {
            cache: Box::leak(Box::new(cache)),
            manifests: dir.join("cache/warm/hooks"),
            base: KeyHasher::new(),
            std_root: dir.join("std"),
            target: String::new(),
            cpu: None,
            linking: true,
            seen: Mutex::default(),
        }
    }

    /// An object found by the check and gone from the cache by the link (another build's
    /// garbage collection, `velt clean --cache`) is still linked: sema has called it by then.
    #[test]
    fn an_object_gone_from_the_cache_by_the_link_is_still_linked() {
        let tmp = tempfile::tempdir().unwrap();
        let finder = finder(tmp.path());
        let hook = prebuilt::of_symbol("velt_hook_prebuilt_probe_text").unwrap();
        let (h, symbol) = (&HOOKS[hook], prebuilt::symbol(&HOOKS[hook]));
        let line = entry_line(
            &symbol,
            &[vir::Ty::F64, vir::Ty::I64, vir::Ty::Ptr],
            vir::Ty::Unit,
        );
        // What a fill stores: the object, and a manifest naming it (no std files).
        let key = finder.lookup_key(h.module_entry);
        let object_key = object_key(&key, &[]);
        let built = tmp.path().join("built.o");
        std::fs::write(&built, b"object bytes").unwrap();
        assert!(finder.cache.put_file(KIND, &object_key, &built));
        let manifest = Manifest {
            object: object_key.hex(),
            entries: vec![line],
            files: vec![],
            tls: vec![],
        };
        write_atomically(&finder.manifest_path(&key), &manifest.render()).unwrap();

        assert!(finder.linked(hook));
        finder.cache.clear().unwrap();
        assert_eq!(finder.cache.path_of(KIND, &object_key), None);
        let program = vir::Program {
            externs: vec![vir::ExternFn {
                symbol,
                params: vec![vir::Ty::F64, vir::Ty::I64, vir::Ty::Ptr],
                ret: vir::Ty::Unit,
                noreturn: false,
                host: None,
                c_decl: None,
                variadic: false,
                c_abi: None,
            }],
            ..vir::Program::default()
        };
        let found = finder.found();
        let (objects, tls) = linked_objects(&program, &found).unwrap();
        assert!(tls.is_empty());
        let place = super::super::Place {
            dir: tmp.path().join("app.std"),
            ext: "o".into(),
        };
        let placed = put(&place, &objects).unwrap();
        assert_eq!(placed, [place.dir.join("hook0.o")]);
        assert_eq!(std::fs::read(&placed[0]).unwrap(), b"object bytes");
        // The pin goes with the build.
        let pinned = found[0].1.object.clone();
        drop(found);
        drop(finder);
        assert!(!pinned.exists());
    }

    /// Set the time `dir` last changed to `by` ago.
    fn backdate(dir: &Path, by: std::time::Duration) {
        #[cfg(unix)]
        let f = std::fs::File::open(dir).unwrap();
        #[cfg(windows)]
        let f = {
            use std::os::windows::fs::OpenOptionsExt;
            // FILE_WRITE_ATTRIBUTES, FILE_FLAG_BACKUP_SEMANTICS (a directory).
            (std::fs::OpenOptions::new().access_mode(0x100))
                .custom_flags(0x0200_0000)
                .open(dir)
                .unwrap()
        };
        f.set_modified(std::time::SystemTime::now() - by).unwrap();
    }

    /// A pin directory a killed build left goes once it is older than any build runs; a fresh
    /// one (a build running now) and other directories stay.
    #[test]
    fn a_stale_pin_directory_is_swept() {
        let tmp = tempfile::tempdir().unwrap();
        let day = std::time::Duration::from_secs(24 * 60 * 60);
        let dir = |name: &str, age| {
            let d = tmp.path().join(name);
            std::fs::create_dir(&d).unwrap();
            std::fs::write(d.join("object"), b"object bytes").unwrap();
            backdate(&d, age);
            d
        };
        let stale = dir("velt-hook-4242-0", day * 2);
        let fresh = dir("velt-hook-4243-0", day / 2);
        let other = dir("other-4242-0", day * 2);
        sweep_stale_pins(tmp.path(), std::time::SystemTime::now());
        assert!(!stale.exists());
        assert!(fresh.join("object").exists());
        assert!(other.join("object").exists());
    }

    /// An object gone before the check is a miss: the build loads the module.
    #[test]
    fn an_object_gone_before_the_check_is_a_miss() {
        let tmp = tempfile::tempdir().unwrap();
        let finder = finder(tmp.path());
        let hook = prebuilt::of_symbol("velt_hook_prebuilt_probe_text").unwrap();
        let key = finder.lookup_key(HOOKS[hook].module_entry);
        let manifest = Manifest {
            object: object_key(&key, &[]).hex(),
            entries: vec![],
            files: vec![],
            tls: vec![],
        };
        write_atomically(&finder.manifest_path(&key), &manifest.render()).unwrap();
        assert!(!finder.linked(hook));
        assert!(finder.found().is_empty());
    }
}
