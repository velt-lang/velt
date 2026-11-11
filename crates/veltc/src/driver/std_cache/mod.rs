//! The std cache (#1728 step 3): std's functions compiled once per variant, stored in the build
//! cache, and linked into every program that compiles the same code (design/std-object.md "The
//! std cache"). Release builds use it by default for the functions std marks `@noInline`,
//! through [`no_inline::serve`], and for all it serves behind `VELT_STD_OBJECT=1`, through
//! [`split`] and [`link`]; debug builds by default, through [`serve::serve`]. Outside
//! `VELT_STD_OBJECT=1`, a miss stays in the program.
//!
//! - Std's code is std's own and what std's generators wrote for the program (`select::is_std`).
//! - After lowering, every std function that may be served (`select`) gets a key (`key`): its
//!   canonical code and parameter attributes, those of every small function it may inline, and
//!   who compiles it for what.
//! - Before the program is optimized ([`split`]), each such function becomes a declaration
//!   (`Import`), and the program's functions its object may call stay visible to it (`Export`).
//!   A key in the build cache (kind `stdfn`, a file entry) is a hit; for a miss, a program of
//!   the function's own is made then (`compile`).
//! - After ([`link`]), the program links the objects of those it still calls (the optimizer
//!   drops a declaration nothing calls any more, as it would have dropped the function); the
//!   misses among them are compiled now, with unknown callers, and stored under their keys.
//!
//! Code with a key is the same in every program that makes that key, so a hit links what this
//! program would have compiled itself, optimized without its callers' facts. If a function
//! cannot be compiled on its own, the build optimizes the program as it was before `split`, with
//! no std objects: the cache costs speed, never correctness.

mod compile;
mod key;
mod no_inline;
mod select;
mod serve;
pub(super) mod stage;
mod statics;
mod turn;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use velt_build_cache::Key;
use velt_vir::fingerprint::{Canon, Mode};
use velt_vir::vir::{self, Linkage};

use super::BuildOptions;

/// The build cache's kind for std functions.
const KIND: &str = "stdfn";
/// A build compiles the functions missing from the cache on their own only when they come to at
/// least this many statements: each costs a clang process (about 30M instructions to start),
/// which a few small functions do not pay back. Below it they stay in the program this time,
/// compiled as usual; a larger build, or the warm-up, stores them. `VELT_STD_OBJECT_MIN_MISSES`
/// overrides it (tests).
const MIN_MISSES: usize = 1000;

fn min_misses() -> usize {
    (std::env::var("VELT_STD_OBJECT_MIN_MISSES").ok())
        .and_then(|v| v.parse().ok())
        .unwrap_or(MIN_MISSES)
}

/// How a build uses the std cache.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Use {
    Off,
    /// [`split`] and [`link`] (`VELT_STD_OBJECT=1`).
    Release,
    /// A release build's default, [`no_inline::serve`]: only what std marks `@noInline`.
    NoInline,
    /// [`serve::serve`].
    Debug,
}

/// How `opts` uses the std cache. Only executables do: an object, VIR or LLVM IR (`--emit`) would
/// name std functions it does not define, and `--report numbers` reads their code. Nor does a
/// `--host` build, whose directory holds what the build writes and nothing else
/// (`commands::host`).
///
/// Release builds: by LLVM without debug info (the cached objects carry no line info, and the
/// cache holds LLVM's code); with `VELT_STD_OBJECT=1` every function the cache serves, unless
/// `VELT_STD_OBJECT=0` those std marks `@noInline`. Debug builds: unless `VELT_STD_OBJECT=0`,
/// with either backend and with `-g`: stepping stays in the program's own code, as the objects
/// have no lines to step through. WebAssembly has no objects to find: its programs compile
/// everything (unless `VELT_STD_OBJECT=1`).
pub(super) fn wanted(opts: &BuildOptions) -> Use {
    if opts.emit != super::Emit::Exe || opts.report_numbers || opts.host.is_some() {
        return Use::Off;
    }
    let setting = std::env::var("VELT_STD_OBJECT");
    if opts.release {
        let llvm = crate::backend::Backend::resolve(opts.backend, opts.release).0
            == crate::backend::Backend::Llvm;
        if opts.debug_info || !llvm {
            return Use::Off;
        }
        let wasm = velt_codegen_llvm::is_wasm(&opts.target());
        return match setting.as_deref() {
            Ok("1") => Use::Release,
            Ok("0") => Use::Off,
            _ if wasm => Use::Off,
            _ => Use::NoInline,
        };
    }
    match setting.is_ok_and(|v| v == "0") || velt_codegen_llvm::is_wasm(&opts.target()) {
        true => Use::Off,
        false => Use::Debug,
    }
}

/// `VELT_STD_OBJECT_STATS`: say what the std cache did, and why std code stayed in the program.
pub(super) fn stats() -> bool {
    std::env::var_os("VELT_STD_OBJECT_STATS").is_some()
}

/// What the program links from the std cache, decided before it is optimized ([`split`]) and
/// done after ([`link`]).
pub(super) struct Plan {
    entries: Vec<Entry>,
}

struct Entry {
    symbol: String,
    object: Object,
    /// The other functions taken out that its object may call (directly, or through a small
    /// function compiled into it): linked whenever it is.
    calls: Vec<String>,
}

enum Object {
    /// In the cache (checked again when linking: another build's garbage collection may have
    /// removed it since).
    Found(Key),
    /// Not yet: the function's program of its own, to compile if the program still calls it.
    Compile(Key, vir::Program),
}

/// Where [`link`] puts the objects it links: `<dir>/<n>.<ext>` (`OutputPaths::std_objects`).
pub(crate) struct Place {
    pub dir: PathBuf,
    pub ext: String,
}

impl Place {
    /// `<dir>.lock`: beside the directory, since the directory itself comes and goes.
    fn lock_path(&self) -> PathBuf {
        let mut path = self.dir.clone().into_os_string();
        path.push(".lock");
        PathBuf::from(path)
    }

    /// Whether a build has got as far as taking this output's turn.
    pub fn lock_exists(&self) -> bool {
        self.lock_path().is_file()
    }

    /// Take this output's turn, held until the file is dropped: builds of one output at once
    /// would remove the objects another is linking. `None` where no lock can be had (a file
    /// system without them), and the build goes on as it would alone. A wait is said on stderr
    /// once it has lasted [`turn::QUIET_WAIT`], or at once with `verbose` (`-v`): a short one is
    /// no news.
    pub fn lock(&self, verbose: bool) -> Option<std::fs::File> {
        let path = self.lock_path();
        let after = match verbose {
            true => std::time::Duration::ZERO,
            false => turn::QUIET_WAIT,
        };
        turn::take(&path, after, || {
            eprintln!(
                "velt: waiting for another build of this output (`{}`)",
                path.display()
            );
        })
    }

    /// Remove what an earlier build put there.
    pub fn clear(&self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }

    /// Link (or copy) the cached file `from` in as object `n`.
    fn put(&self, n: usize, from: &std::path::Path) -> Result<PathBuf, String> {
        let to = self.dir.join(format!("{n}.{}", self.ext));
        let placed = std::fs::create_dir_all(&self.dir).and_then(|()| {
            std::fs::hard_link(from, &to).or_else(|_| std::fs::copy(from, &to).map(drop))
        });
        placed
            .map(|()| to)
            .map_err(|e| format!("{}: {e}", from.display()))
    }
}

/// What [`link`] did.
#[derive(Default)]
pub(super) struct Linked {
    /// The objects of the std functions taken out of the program, for the link.
    pub objects: Vec<PathBuf>,
    /// Functions found in the cache, compiled now, and taken out but no longer called (after
    /// optimization, as the program would have dropped them).
    pub hits: usize,
    pub misses: usize,
    pub dropped: usize,
}

/// Take the std functions the cache serves out of `program` (lowered, not optimized): each
/// becomes a declaration. For those not in the cache, their programs of their own are made now,
/// from this program; [`link`] compiles them once the program is optimized.
pub(super) fn split(
    program: &mut vir::Program,
    opts: &BuildOptions,
    cg: &velt_codegen_cl::CodegenOptions,
) -> Plan {
    let mut plan = Plan { entries: vec![] };
    let Some(cache) = crate::build_cache::cache() else {
        return plan;
    };
    let canon = Canon::new(program, Mode::Link);
    let names = statics::Names::new(program, &canon);
    if stats() {
        eprintln!(
            "velt: std cache: std statements by reason: {:?}",
            select::reasons(program, &names, select::Rule::Release)
        );
    }
    let candidates = select::candidates(program, &names, select::Rule::Release);
    if candidates.is_empty() {
        return plan;
    }
    let inputs = key::Inputs::of(opts, cg);
    let mut memo = HashMap::new();
    let looked_up: Vec<(usize, Key, Option<PathBuf>)> = (candidates.iter())
        .map(|&i| {
            let key = release_key(program, &canon, &inputs, i, DEPTH_ALL, PROGRAM, &mut memo);
            (i, key, cache.path_of(KIND, &key))
        })
        .collect();
    drop(canon);
    let missing: usize = (looked_up.iter())
        .filter(|(_, _, found)| found.is_none())
        .map(|&(i, _, _)| select::size(&program.funcs[i]))
        .sum();
    let compile_misses = missing >= min_misses();
    let looked_up: Vec<_> = (looked_up.into_iter())
        .filter(|(_, _, found)| found.is_some() || compile_misses)
        .collect();
    let taken: HashSet<usize> = looked_up.iter().map(|&(i, _, _)| i).collect();
    for (i, key, found) in looked_up {
        let object = match found {
            Some(_) => Object::Found(key),
            None => {
                let alone = compile::program_of(program, i, &names, DEPTH_ALL, PROGRAM);
                Object::Compile(key, alone)
            }
        };
        let symbol = program.funcs[i].symbol.clone();
        let mut calls: Vec<String> = (select::inlinable(program, i, DEPTH_ALL).into_iter())
            .flat_map(|j| select::referenced(&program.funcs[j]))
            .filter(|c| *c != i && taken.contains(c))
            .map(|c| program.funcs[c].symbol.clone())
            .collect();
        calls.sort();
        calls.dedup();
        plan.entries.push(Entry {
            symbol,
            object,
            calls,
        });
    }
    let mut taken: Vec<usize> = taken.into_iter().collect();
    taken.sort_unstable();
    // Every set as keyed, before anything is exported: `inlinable` follows internal functions
    // only, and exporting one would hide it from the sets after.
    let linked: Vec<(usize, Vec<usize>)> = (taken.iter())
        .map(|&i| (i, select::inlinable(program, i, DEPTH_ALL)))
        .collect();
    for (_, set) in &linked {
        let reached = statics::reached(program, set);
        statics::export(program, &names, &reached);
    }
    if stats() {
        let named = program
            .statics
            .iter()
            .filter(|s| s.symbol.is_some())
            .count();
        eprintln!("velt: std cache: {named} statics named");
    }
    take_out(program, &linked, DEPTH_ALL);
    plan
}

/// What [`split`]'s objects (`VELT_STD_OBJECT=1`) are compiled with: the small functions they
/// call, as before release builds served `@noInline` functions by default.
const DEPTH_ALL: select::Depth = select::Depth::Small;

/// The parameter attributes [`split`]'s objects assume: the program's. It compiles its misses
/// itself, so it has no use for objects that other programs could link too.
const PROGRAM: key::Assumed = key::Assumed::Program;

/// A release build's key of std function `i`: at `Depth::Whole` a mark that says so, then its
/// code and what it is compiled with at `depth` (`select::inlinable`), then what its object
/// declares (`select::declared`), each with the parameter attributes `assumed` keeps. `memo`:
/// each function's canonical text, worked out once per build (a function is in the set of
/// everything that reaches it, and in both of its keys). The mark keeps apart two objects of one set: at `Depth::Small` the
/// object calls the program's copies of what it doesn't inline, at `Depth::Whole` its own, which
/// the program then doesn't export.
fn release_key(
    program: &vir::Program,
    canon: &Canon,
    inputs: &key::Inputs,
    i: usize,
    depth: select::Depth,
    assumed: key::Assumed,
    memo: &mut HashMap<usize, String>,
) -> Key {
    let set = select::inlinable(program, i, depth);
    let declared = select::declared(program, &set);
    let mut code: Vec<key::Code> = vec![];
    if depth == select::Depth::Whole {
        code.push(key::Code {
            symbol: String::new(),
            text: "own copies of every callee up to @noInline".into(),
            attrs: String::new(),
        });
    }
    for &j in &set {
        let f = &program.funcs[j];
        let text = memo.entry(j).or_insert_with(|| canon.text(f));
        code.push(key::Code::with_text(f, text.clone(), assumed));
    }
    code.extend((declared.iter()).map(|&j| key::Code::declared(&program.funcs[j], assumed)));
    inputs.key(code)
}

/// After `program` is optimized: link what it still calls of what [`split`] took out, compiling
/// (in parallel) and storing the functions not yet in the cache, and placing each object in
/// `place` as soon as it is found or stored. `Err` when one of them could not be compiled on its
/// own, or is gone from the cache by then (another build's garbage collection, `velt clean
/// --cache`): the build then optimizes the program as it was before `split`.
pub(super) fn link(
    plan: Plan,
    program: &vir::Program,
    cg: &velt_codegen_cl::CodegenOptions,
    place: &Place,
) -> Result<Linked, String> {
    match crate::build_cache::cache() {
        Some(cache) => link_with(plan, program, cg, cache, place),
        None => Ok(Linked::default()),
    }
}

/// [`link`] with `cache`.
fn link_with(
    plan: Plan,
    program: &vir::Program,
    cg: &velt_codegen_cl::CodegenOptions,
    cache: &velt_build_cache::Cache,
    place: &Place,
) -> Result<Linked, String> {
    let mut out = Linked::default();
    place.clear();
    // What the program still calls, and what those objects call in turn.
    let mut needed: HashSet<&str> = (program.funcs.iter())
        .filter(|f| f.linkage == Linkage::Import)
        .map(|f| f.symbol.as_str())
        .collect();
    let by_symbol: std::collections::HashMap<&str, &Entry> = plan
        .entries
        .iter()
        .map(|e| (e.symbol.as_str(), e))
        .collect();
    let mut pending: Vec<&str> = needed.iter().copied().collect();
    while let Some(s) = pending.pop() {
        for c in by_symbol.get(s).map_or(&[][..], |e| &e.calls[..]) {
            if needed.insert(c.as_str()) {
                pending.push(c.as_str());
            }
        }
    }
    let called: HashSet<String> = needed.into_iter().map(str::to_string).collect();
    drop(by_symbol);
    let mut compile: Vec<(String, Key)> = vec![];
    let mut programs: Vec<vir::Program> = vec![];
    for entry in plan.entries {
        if !called.contains(entry.symbol.as_str()) {
            out.dropped += 1;
            continue;
        }
        match entry.object {
            Object::Found(key) => {
                let path = (cache.path_of(KIND, &key))
                    .ok_or_else(|| format!("{}: no longer in the cache", entry.symbol))?;
                out.objects.push(place.put(out.objects.len(), &path)?);
                out.hits += 1;
            }
            Object::Compile(key, alone) => {
                compile.push((entry.symbol, key));
                programs.push(alone);
            }
        }
    }
    let compiled = compile::each(programs, cg);
    for ((symbol, key), object) in compile.iter().zip(compiled) {
        let object = object.map_err(|e| format!("{symbol}: {e}"))?;
        let path = compile::store(cache, key, &object)
            .ok_or_else(|| format!("{symbol}: its object could not be stored"))?;
        out.objects.push(place.put(out.objects.len(), &path)?);
        out.misses += 1;
    }
    Ok(out)
}

/// Make the functions `linked` declarations, and every function of the program their objects may
/// refer to visible to them: what they declare, and what they are compiled with at `depth` but
/// have no copy of their own of (`compile::own_copies`), whose bodies are the program's. Each
/// comes with its set (`select::inlinable`) as keyed: worked out before `statics::export` made
/// any function `Export`, which `inlinable` would no longer follow.
fn take_out(program: &mut vir::Program, linked: &[(usize, Vec<usize>)], depth: select::Depth) {
    expose(program, linked, depth);
    for &(i, _) in linked {
        declare(&mut program.funcs[i]);
    }
}

/// [`take_out`]'s first half: every function of the program the objects of `linked` may refer to
/// made visible to them (`Export`). Before the program is optimized, so that the optimizer treats
/// them as having unknown callers (no facts from the program's calls into them).
fn expose(program: &mut vir::Program, linked: &[(usize, Vec<usize>)], depth: select::Depth) {
    let linked_set: HashSet<usize> = linked.iter().map(|(i, _)| *i).collect();
    let mut visible: HashSet<usize> = HashSet::new();
    for (_, set) in linked {
        let reached = statics::reached(program, set);
        let own = compile::own_copies(program, set, &reached, depth);
        let shared = set.iter().skip(1).filter(|j| !own.contains(j));
        visible.extend(shared.chain(&select::declared(program, set)));
    }
    for c in visible {
        let f = &mut program.funcs[c];
        if !linked_set.contains(&c) && f.linkage == Linkage::Internal {
            f.linkage = Linkage::Export;
        }
    }
}

/// [`take_out`]'s second half: `f` a declaration (`Import`), its object linked instead.
fn declare(f: &mut vir::Function) {
    if stats() {
        let size: usize = f.blocks.iter().map(|b| b.stmts.len() + 1).sum();
        eprintln!("velt: std cache: links {} ({size} statements)", f.symbol);
    }
    f.blocks.clear();
    f.locs.clear();
    f.locals.truncate(f.params.len());
    f.linkage = Linkage::Import;
}

#[cfg(test)]
mod tests {
    use super::*;
    use velt_vir::vir::{BasicBlock, Callee, FuncId, Function, Local, Operand, Place, Terminator};

    fn calling(name: &str, callee: Option<u32>, stmts: usize) -> Function {
        let mut f = Function::new(name.into(), vec![], vir::Ty::I64, Linkage::Internal);
        f.locals.push(vir::LocalDecl::new(vir::Ty::I64, None));
        let nop = || vir::Stmt::Nop;
        let mut blocks = vec![];
        if let Some(c) = callee {
            blocks.push(BasicBlock {
                stmts: (0..stmts).map(|_| nop()).collect(),
                term: Terminator::Call {
                    callee: Callee::Func(FuncId(c)),
                    args: vec![],
                    dest: Some(Place::local(Local(0))),
                    next: vir::BlockId(1),
                    unwind: None,
                },
            });
        }
        blocks.push(BasicBlock {
            stmts: vec![],
            term: Terminator::Return(Operand::Copy(Place::local(Local(0)))),
        });
        f.blocks = blocks;
        f
    }

    /// What a linked function's object may call back stays in the program, visible to it: what
    /// it calls, and what the small functions it may have inlined call. The function itself
    /// becomes a declaration.
    /// An entry found before the program was optimized but gone by the link (another build's
    /// garbage collection, `velt clean --cache`) is an `Err`, which makes the build fall back;
    /// one still there is placed under an object file's name.
    #[test]
    fn an_entry_gone_by_the_link_is_an_error_not_a_failed_build() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = velt_build_cache::Cache::new(&tmp.path().join("cache"), u64::MAX);
        let program = vir::Program {
            funcs: vec![Function::new(
                "_V3stdN1f".into(),
                vec![],
                vir::Ty::I64,
                Linkage::Import,
            )],
            ..vir::Program::default()
        };
        let key = Key([9; 32]);
        let plan = || super::Plan {
            entries: vec![super::Entry {
                symbol: "_V3stdN1f".into(),
                object: super::Object::Found(key),
                calls: vec![],
            }],
        };
        let cg = velt_codegen_cl::CodegenOptions {
            target: velt_codegen_cl::host_triple(),
            optimize: true,
            cpu: None,
        };
        let place = super::Place {
            dir: tmp.path().join("app.std"),
            ext: "obj".into(),
        };
        let gone = link_with(plan(), &program, &cg, &cache, &place)
            .err()
            .unwrap();
        assert!(gone.contains("no longer in the cache"), "{gone}");
        let object = tmp.path().join("f.o");
        std::fs::write(&object, b"object bytes").unwrap();
        assert!(cache.put_file(KIND, &key, &object));
        let linked = link_with(plan(), &program, &cg, &cache, &place).unwrap();
        assert_eq!(linked.objects, [place.dir.join("0.obj")]);
        assert_eq!(std::fs::read(&linked.objects[0]).unwrap(), b"object bytes");
    }

    /// One set compiled at both depths makes two objects (the program's copies of the callees, or
    /// the object's own), so their keys differ.
    #[test]
    fn the_depth_is_in_a_release_key() {
        let mut f = calling("_V3stdN1f", None, 0);
        f.inline = vir::Inline::Never;
        let p = vir::Program {
            funcs: vec![f],
            ..vir::Program::default()
        };
        let canon = Canon::new(&p, velt_vir::fingerprint::Mode::Link);
        let inputs = key::Inputs {
            compiler: velt_build_cache::Key([1; 32]),
            backend: "clang".into(),
            opt: "-O3".into(),
            target: "aarch64-apple-darwin".into(),
            cpu: None,
            release: true,
        };
        let at = |depth| release_key(&p, &canon, &inputs, 0, depth, PROGRAM, &mut HashMap::new());
        assert_eq!(select::inlinable(&p, 0, select::Depth::Small), [0]);
        assert_eq!(select::inlinable(&p, 0, select::Depth::Whole), [0]);
        assert_ne!(at(select::Depth::Small), at(select::Depth::Whole));
        assert_eq!(at(select::Depth::Whole), at(select::Depth::Whole));
    }

    #[test]
    fn what_linked_code_may_call_is_exported() {
        let mut p = vir::Program {
            funcs: vec![
                calling("std_big", Some(1), 60),
                calling("small", Some(2), 1),
                calling("behind_small", None, 0),
                calling("unrelated", None, 0),
            ],
            ..vir::Program::default()
        };
        let set = select::inlinable(&p, 0, select::Depth::Small);
        take_out(&mut p, &[(0, set)], select::Depth::Small);
        let linkage: Vec<Linkage> = p.funcs.iter().map(|f| f.linkage).collect();
        assert_eq!(
            linkage,
            [
                Linkage::Import,
                Linkage::Export,
                Linkage::Export,
                Linkage::Internal
            ]
        );
        assert!(p.funcs[0].blocks.is_empty() && p.funcs[0].locals.is_empty());
    }
}
