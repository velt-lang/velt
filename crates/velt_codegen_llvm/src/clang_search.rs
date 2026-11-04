//! Choosing the `clang` the backend uses.
//!
//! `$VELT_CLANG` is final: if it can't be used, no other clang is tried behind the user's back.
//!
//! On macOS the newest clang found wins, because Apple's clang makes worse choices than a newer
//! LLVM on the same IR: every `clang` on `PATH`, Homebrew's `llvm`, `/usr/local/bin/clang` and every
//! versioned `clang-NN` on `PATH` are ranked by the version they report, and `/usr/bin/clang`
//! comes last. Apple numbers its releases on its own scheme, so its versions can't be compared
//! with LLVM's: any clang from LLVM ranks above Apple's. The `/usr/bin/clang` shim is not even
//! started when another clang will do (on a Mac without developer tools it opens an install
//! dialog).
//!
//! Elsewhere the first usable clang in a fixed order wins: `clang` on `PATH`, the standard install
//! locations, then `clang-NN` from new to old. There, `clang` is what the user or the distribution
//! chose as the default (`update-alternatives`), and the reason to overrule it on macOS, a default
//! that can't be changed and lags LLVM, does not apply.
//!
//! A candidate counts only if `clang --version` answers in time and reports LLVM 16 or newer: the
//! emitted IR uses opaque `ptr` types (LLVM 15+) and `memory(...)` attributes (LLVM 16+), and Linux
//! distributions often ship an older default `clang` (Ubuntu 20.04: 10, 22.04: 14) next to a newer
//! `clang-NN`. The one about to be chosen must also compile for this machine, which a toolchain for
//! another target only (wasi-sdk) doesn't. One that fails is passed over for the next; if none
//! compiles, the first is kept, since it may still do for the target a build names. What a clang
//! does later is not retried on another: one build's objects come from the clang its keys name
//! (`clang::compile_cached` says what went wrong, and the next build asks again). The result is
//! cached per process.

use std::cmp::Reverse;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::clang_probe::{
    compiles, named_clang, probe, resolve_in, runnable, ClangVersion, Probe, MIN_LLVM_MAJOR,
    PROBE_LIMIT, SYSTEM_CLANG,
};

/// The newest `clang-NN` looked for on `PATH`.
const NEWEST_VERSIONED_CLANG: u32 = 30;

/// Whether a working clang was found, i.e. whether [`crate::emit_object`] can work.
pub fn available() -> bool {
    find_clang().is_some()
}

/// The clang executable used by the backend, if any.
pub fn find_clang() -> Option<PathBuf> {
    search_result().found.as_ref().map(|f| f.path.clone())
}

/// What the clang in use says it is: the first line of its `--version`. Part of the key of
/// everything compiled with it, since a new clang can sit behind an unchanged path, size and time
/// (Homebrew's `opt/llvm` symlink, Apple's shim).
pub fn clang_version() -> Option<String> {
    search_result().found.as_ref().map(|f| f.banner.clone())
}

/// When no usable clang was found but one was rejected, why (for diagnostics).
pub fn rejected_clang() -> Option<String> {
    let search = search_result();
    if search.found.is_some() {
        return None;
    }
    search.rejected.clone()
}

/// Take back what is known about the clang in use, because it failed a compile as small as they
/// come: the next build asks it again.
pub(crate) fn forget_clang() {
    if let Some(found) = &search_result().found {
        crate::clang_probe::forget(&found.path, &found.banner);
    }
}

/// Which clang is in use, its version and why it was chosen (`velt doctor`).
pub fn describe_clang() -> Option<String> {
    search_result().describe()
}

/// A candidate that can be used.
#[derive(Debug)]
struct Found {
    path: PathBuf,
    /// The first line of its `--version`.
    banner: String,
    /// `None` when that line isn't one [`parse_version`] knows.
    version: Option<ClangVersion>,
}

impl Found {
    fn describe(&self) -> String {
        let version = (self.version.as_ref()).map_or("version unknown".into(), |v| v.describe());
        format!("{} ({version})", self.path.display())
    }
}

/// Why the search ended on the clang it found.
#[derive(Debug, PartialEq, Eq)]
enum Why {
    Explicit,
    FirstInOrder,
    Newest,
    /// macOS: nothing but `/usr/bin/clang` can be used.
    OnlySystem,
    /// macOS: nothing else was found, and this one's version isn't one [`parse_version`] knows.
    ///
    /// [`parse_version`]: crate::clang_probe::parse_version
    OnlyUnknown,
    /// None of them compiles for this machine: this one is the first.
    NoneCompiles,
}

/// Outcome of the (cached) clang search.
#[derive(Debug)]
struct Search {
    found: Option<Found>,
    why: Why,
    /// The first candidate that runs but can't be used, described.
    rejected: Option<String>,
    /// The other candidates and why they lost, for `velt doctor`.
    passed_over: Vec<String>,
}

impl Search {
    fn describe(&self) -> Option<String> {
        let found = self.found.as_ref()?;
        let why = match self.why {
            Why::Explicit => "set by $VELT_CLANG",
            Why::FirstInOrder => "the first usable clang in the search order",
            Why::Newest => "the newest clang found",
            Why::OnlySystem => {
                "no other usable clang found; a newer LLVM makes faster code for some loops \
                 (`brew install llvm`)"
            }
            Why::OnlyUnknown => "the only usable clang found; it doesn't say which version it is",
            Why::NoneCompiles => "no clang found compiles for this machine; this is the first",
        };
        let mut text = format!("{}: {why}", found.describe());
        for other in &self.passed_over {
            text.push_str("; ");
            text.push_str(other);
        }
        Some(text)
    }
}

fn search_result() -> &'static Search {
    static SEARCH: OnceLock<Search> = OnceLock::new();
    SEARCH.get_or_init(search)
}

fn search() -> Search {
    let path = std::env::var_os("PATH");
    let path = path.as_deref();
    // An explicit choice is final: silently using another clang would be surprising. It isn't
    // checked for this machine either: it may be a cross compiler for the target a build names.
    if let Some(clang) = std::env::var_os("VELT_CLANG").filter(|p| !p.is_empty()) {
        let ask = Ask {
            version: &probe,
            compiles: &|_, _| Ok(()),
        };
        return first_usable(vec![PathBuf::from(clang)], Why::Explicit, path, &ask);
    }
    let ask = Ask {
        version: &probe,
        compiles: &compiles,
    };
    if cfg!(target_os = "macos") {
        let system = runnable(PathBuf::from(SYSTEM_CLANG));
        return newest(
            ranked_candidates(path, Path::new(SYSTEM_CLANG)),
            system,
            &ask,
        );
    }
    first_usable(candidates(), Why::FirstInOrder, path, &ask)
}

/// What the search wants to know of a candidate.
struct Ask<'a> {
    /// What it says it is.
    version: &'a dyn Fn(&Path) -> Probe,
    /// Whether the candidate, with the banner it gave, compiles for this machine; else why not.
    compiles: &'a dyn Fn(&Path, &str) -> Result<(), String>,
}

/// What the search keeps of the candidates it passed over.
#[derive(Default)]
struct Notes {
    rejected: Option<String>,
    passed_over: Vec<String>,
    /// The first that says it can be used but doesn't compile for this machine.
    cannot_compile: Option<Found>,
}

impl Notes {
    /// `clang`, if what it says it is will do. One that doesn't run is noted if `known_file`.
    fn version(&mut self, clang: PathBuf, ask: &Ask, known_file: bool) -> Option<Found> {
        match (ask.version)(&clang) {
            Probe::Usable { banner, version } => {
                return Some(Found {
                    path: clang,
                    banner,
                    version,
                })
            }
            Probe::TooOld(version) => {
                self.reject(format!(
                    "`{}` is {version}, but the LLVM backend needs clang {MIN_LLVM_MAJOR} or newer",
                    clang.display()
                ));
            }
            Probe::TimedOut => {
                self.reject(format!(
                    "`{}` did not answer `--version` within {} s",
                    clang.display(),
                    PROBE_LIMIT.as_secs()
                ));
            }
            Probe::Missing if known_file => {
                (self.passed_over).push(format!("`{}` does not run", clang.display()));
            }
            Probe::Missing => {}
        }
        None
    }

    fn reject(&mut self, text: String) {
        self.passed_over.push(text.clone());
        self.rejected.get_or_insert(text);
    }

    /// `found`, if it compiles for this machine.
    fn compiles(&mut self, found: Found, ask: &Ask) -> Option<Found> {
        match (ask.compiles)(&found.path, &found.banner) {
            Ok(()) => Some(found),
            Err(why) => {
                (self.passed_over).push(format!(
                    "{} can't compile for this machine: {why}",
                    found.describe()
                ));
                self.cannot_compile.get_or_insert(found);
                None
            }
        }
    }
}

/// The first of `list` that can be used. A bare name means the file starting it would start, so
/// that what is asked is what the build runs.
fn first_usable(list: Vec<PathBuf>, why: Why, path: Option<&OsStr>, ask: &Ask) -> Search {
    let mut notes = Notes::default();
    for name in list {
        let file = resolve_in(&name, path);
        let known_file = file.is_some();
        let clang = file.unwrap_or(name);
        let found = notes
            .version(clang, ask, known_file)
            .and_then(|found| notes.compiles(found, ask));
        if let Some(found) = found {
            return Search {
                found: Some(found),
                why,
                rejected: None,
                passed_over: notes.passed_over,
            };
        }
    }
    let found = notes.cannot_compile;
    Search {
        why: if found.is_some() {
            Why::NoneCompiles
        } else {
            why
        },
        rejected: if found.is_some() {
            None
        } else {
            notes.rejected
        },
        passed_over: if found.is_some() {
            notes.passed_over
        } else {
            vec![]
        },
        found,
    }
}

/// The newest of `list` that can be used, or `system` when none of `list` from LLVM can and it
/// outranks the rest. `system` isn't started when one from LLVM will do.
fn newest(list: Vec<PathBuf>, system: Option<PathBuf>, ask: &Ask) -> Search {
    let mut notes = Notes::default();
    let mut usable: Vec<Found> = vec![];
    for clang in list {
        usable.extend(notes.version(clang, ask, true));
    }
    sort_by_rank(&mut usable);
    let mut found = first_compiling(&mut usable, true, ask, &mut notes);
    let mut unasked = None;
    match system {
        Some(system) if found.is_some() => unasked = Some(system),
        Some(system) => usable.extend(notes.version(system, ask, true)),
        None => {}
    }
    if found.is_none() {
        sort_by_rank(&mut usable);
        found = first_compiling(&mut usable, false, ask, &mut notes);
    }
    (notes.passed_over).extend(
        usable
            .iter()
            .map(|f| format!("also found {}", f.describe())),
    );
    if let Some(system) = unasked {
        (notes.passed_over).push(format!("{} is the last resort", system.display()));
    }
    let none_compiles = found.is_none() && notes.cannot_compile.is_some();
    let found = found.or(notes.cannot_compile);
    let why = match &found {
        _ if none_compiles => Why::NoneCompiles,
        Some(f) if f.path == Path::new(SYSTEM_CLANG) => Why::OnlySystem,
        Some(f) if rank(f).0 == Rank::Unknown => Why::OnlyUnknown,
        _ => Why::Newest,
    };
    Search {
        rejected: if found.is_some() {
            None
        } else {
            notes.rejected
        },
        found,
        why,
        passed_over: notes.passed_over,
    }
}

fn sort_by_rank(usable: &mut [Found]) {
    usable.sort_by_key(|f| Reverse(rank(f))); // Stable: on a tie, the earlier candidate.
}

/// Take the first of `usable` (ranked) that compiles, of those from LLVM if `llvm_only`.
fn first_compiling(
    usable: &mut Vec<Found>,
    llvm_only: bool,
    ask: &Ask,
    notes: &mut Notes,
) -> Option<Found> {
    while let Some(i) = (usable.iter()).position(|f| !llvm_only || rank(f).0 == Rank::Llvm) {
        if let Some(found) = notes.compiles(usable.remove(i), ask) {
            return Some(found);
        }
    }
    None
}

/// How candidates of different origins compare: a version only means something within one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Rank {
    /// Its `--version` is one [`parse_version`] doesn't know: usable, but nothing says it's new.
    Unknown,
    Apple,
    Llvm,
}

fn rank(found: &Found) -> (Rank, (u32, u32, u32)) {
    match &found.version {
        None => (Rank::Unknown, (0, 0, 0)),
        Some(v) if v.apple => (Rank::Apple, v.number),
        Some(v) => (Rank::Llvm, v.number),
    }
}

/// The macOS candidates other than `system`, as the files they name, each clang once. Every match
/// on `path` counts, not only the first: the first `clang` is often Apple's in `/usr/bin`, shadowing
/// the one the user installed. That is one `stat` per name and directory, each build; only a clang
/// not seen before costs more, once, for its first `--version`.
fn ranked_candidates(path: Option<&OsStr>, system: &Path) -> Vec<PathBuf> {
    let on_path = |name: String| -> Vec<PathBuf> {
        let dirs = path.map(std::env::split_paths).into_iter().flatten();
        dirs.filter_map(|dir| runnable(dir.join(&name))).collect()
    };
    let known = [
        "/opt/homebrew/opt/llvm/bin/clang",
        "/usr/local/opt/llvm/bin/clang",
        "/usr/local/bin/clang",
    ]
    .map(PathBuf::from)
    .into_iter()
    .filter_map(runnable);
    let versioned = (MIN_LLVM_MAJOR..=NEWEST_VERSIONED_CLANG)
        .rev()
        .flat_map(|version| on_path(format!("clang-{version}")));
    let mut seen = vec![one_clang(system)];
    let mut list = vec![];
    for file in on_path("clang".into())
        .into_iter()
        .chain(known)
        .chain(versioned)
    {
        let clang = one_clang(&file);
        if !seen.contains(&clang) {
            seen.push(clang);
            list.push(file);
        }
    }
    list
}

/// What makes `file` one clang: the file it links to, and the name it is started as if that file
/// isn't named clang, as it may then be a program that picks what to run by that name (ccache).
fn one_clang(file: &Path) -> (PathBuf, Option<OsString>) {
    let real = file.canonicalize().unwrap_or_else(|_| file.into());
    let name = (!named_clang(&real))
        .then(|| file.file_name().map(OsString::from))
        .flatten();
    (real, name)
}

/// The candidates outside macOS, in order.
fn candidates() -> Vec<PathBuf> {
    let exe = if cfg!(windows) { "clang.exe" } else { "clang" };
    let mut list = vec![PathBuf::from(exe)];
    if cfg!(windows) {
        for var in ["ProgramFiles", "ProgramW6432"] {
            if let Some(dir) = std::env::var_os(var) {
                list.push(Path::new(&dir).join("LLVM").join("bin").join(exe));
            }
        }
        list.push(PathBuf::from(r"C:\Program Files\LLVM\bin\clang.exe"));
    } else {
        for dir in [
            "/opt/homebrew/opt/llvm/bin",
            "/usr/local/opt/llvm/bin",
            "/usr/local/bin",
            "/usr/bin",
        ] {
            list.push(Path::new(dir).join("clang"));
        }
        for version in (MIN_LLVM_MAJOR..=NEWEST_VERSIONED_CLANG).rev() {
            list.push(PathBuf::from(format!("clang-{version}")));
        }
    }
    list
}

#[cfg(test)]
#[path = "clang_search_tests.rs"]
mod tests;
