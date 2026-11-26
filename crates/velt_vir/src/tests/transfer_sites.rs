//! Where lowering can start a transfer to another thread (std's transfer map, #3584): each call
//! of a transfer-starting entry point in lowering's sources, by the function it is in, with the
//! live construct sema marks `Construct::Transfer` for (velt_sema `lazy_std/live_walk.rs`) or the
//! entry point that already covers it. A begin where sema did not demand the construct is an
//! ICE (lower/transfer_map.rs), but only in a program that reaches it: this list makes a new site
//! fail here, in every build, so its construct is marked before a program no golden covers
//! reaches it. A site that is gone fails too, so the list stays the whole of them.

use std::collections::BTreeSet;
use std::path::Path;

use velt_sema::hir::{self, Intrinsic as I};

use super::{lower_ok, lower_src, std_src};

/// The calls that start a transfer, or lead to one: the begins of std's map and lowering's entry
/// points that reach them.
const ENTRIES: &[&str] = &[
    ".xfer_begin()",
    ".xfer_keep_begin()",
    ".xfer_share_begin()",
    "Glue::TransferRoot,",
    ".thread_copy(",
    ".clone_keeping_identity(",
    ".many_walk(",
    ".transfer_value(",
    ".transfer_copy(",
    ".reader_copy(",
    ".reader_keep(",
    ".transfer_root_in_place(",
    ".maybe_transfer(",
    ".transfer_stored(",
    ".share_stored(",
];

/// What makes sema demand `Construct::Transfer` wherever a site runs (velt_sema
/// `transfer_starts`, which the live walk marks it with).
enum Cover {
    /// A call of one of these intrinsics, each of `transfer_starts::INTRINSICS`.
    By(&'static [I]),
    /// An async closure that copies its captures for each call (`transfer_starts::copies_captures`).
    AsyncClosure,
    /// Only reached through this listed entry (the function is that entry itself, or glue it
    /// builds), whose other sites are covered.
    Through(&'static str),
}
use Cover::{AsyncClosure, By, Through};

/// `spawn`, with its result handled or not.
const SPAWN: Cover = By(&[I::Spawn, I::SpawnHandled]);
/// A `read` callback: an `RwLock`'s and a shared value's.
const READS: Cover = By(&[I::LockRead, I::SharedRead]);
/// A lock's callback whose result leaves the lock.
const LOCKS: Cover = By(&[I::MutexWith, I::LockRead, I::SharedRead]);
/// A `Mutex.with` or an `RwLock.write` (and the read lowered with them).
const WITH: Cover = By(&[I::MutexWith, I::LockRead]);
const SHARED: Cover = By(&[I::SharedNew]);
const STORED: Cover = By(&[I::Transfer]);
const PUT: Cover = By(&[I::SlotPut]);
const MOVED: Cover = By(&[I::SlotTransfer]);
/// The glue that transfers a value from its root.
const ROOT: Cover = Through("Glue::TransferRoot,");

/// `(file, function, entry, cover)` for every call of an entry.
const SITES: &[(&str, &str, &str, Cover)] = &[
    ("adt.rs", "shared_new", ".many_walk(", SHARED),
    ("adt.rs", "shared_new", ".transfer_value(", SHARED),
    (
        "async_fn/ctor.rs",
        "state_from_closure",
        ".maybe_transfer(",
        SPAWN,
    ),
    (
        "async_fn/ctor.rs",
        "take_capture",
        ".clone_keeping_identity(",
        AsyncClosure,
    ),
    ("async_fn/spawn.rs", "spawn", "Glue::TransferRoot,", SPAWN),
    (
        "async_fn/spawn.rs",
        "transfer_result",
        "Glue::TransferRoot,",
        SPAWN,
    ),
    (
        "async_fn/suspend.rs",
        "async_args",
        ".maybe_transfer(",
        SPAWN,
    ),
    ("async_fn/sync.rs", "leave_lock", ".reader_keep(", LOCKS),
    ("async_fn/sync.rs", "leave_lock", ".transfer_value(", LOCKS),
    ("async_fn/sync.rs", "mutex_with", ".many_walk(", WITH),
    (
        "async_fn/sync.rs",
        "mutex_with",
        ".transfer_root_in_place(",
        WITH,
    ),
    ("call.rs", "borrow_args", ".transfer_copy(", SPAWN),
    ("callee.rs", "call_indirect_in", ".transfer_value(", SPAWN),
    ("caller_stores.rs", "copy_mut_arg", ".reader_copy(", READS),
    (
        "caller_stores.rs",
        "count_for_caller",
        ".reader_keep(",
        READS,
    ),
    ("caller_stores.rs", "keep_mut_arg", ".reader_keep(", READS),
    ("caller_stores.rs", "keep_value", ".reader_copy(", READS),
    ("caller_stores.rs", "keep_value", ".reader_keep(", READS),
    (
        "caller_stores.rs",
        "reader_keep",
        ".xfer_keep_begin()",
        Through(".reader_keep("),
    ),
    (
        "caller_stores.rs",
        "reader_keep",
        "Glue::TransferRoot,",
        Through(".reader_keep("),
    ),
    ("closure.rs", "fill_env_from", ".maybe_transfer(", SPAWN),
    ("dispatch.rs", "receiver", ".transfer_copy(", SPAWN),
    (
        "glue/many.rs",
        "many_walk",
        ".xfer_begin()",
        Through(".many_walk("),
    ),
    (
        "glue/transfer.rs",
        "transfer_promise",
        "Glue::TransferRoot,",
        ROOT,
    ),
    (
        "glue/transfer.rs",
        "transfer_root_body",
        ".xfer_begin()",
        ROOT,
    ),
    (
        "glue/transfer_env.rs",
        "answer_call_copy",
        ".xfer_begin()",
        SHARED,
    ),
    ("glue/transfer_env.rs", "own_cell", ".thread_copy(", ROOT),
    ("intrinsics.rs", "intrinsic", ".transfer_stored(", STORED),
    (
        "leave_levels.rs",
        "share_stored",
        ".transfer_value(",
        Through(".share_stored("),
    ),
    (
        "leave_levels.rs",
        "share_stored",
        ".xfer_share_begin()",
        Through(".share_stored("),
    ),
    (
        "leave_levels.rs",
        "transfer_stored",
        ".share_stored(",
        STORED,
    ),
    ("slot.rs", "slot_put", ".transfer_value(", PUT),
    ("slot.rs", "slot_transfer", ".transfer_value(", MOVED),
    (
        "transfer.rs",
        "clone_keeping_identity",
        ".xfer_begin()",
        Through(".clone_keeping_identity("),
    ),
    (
        "transfer.rs",
        "maybe_transfer",
        ".transfer_value(",
        Through(".maybe_transfer("),
    ),
    (
        "transfer.rs",
        "reader_copy",
        "Glue::TransferRoot,",
        Through(".reader_copy("),
    ),
    (
        "transfer.rs",
        "thread_copy",
        ".xfer_begin()",
        Through(".thread_copy("),
    ),
    (
        "transfer.rs",
        "transfer_copy",
        ".thread_copy(",
        Through(".transfer_copy("),
    ),
    (
        "transfer.rs",
        "transfer_root_in_place",
        "Glue::TransferRoot,",
        Through(".transfer_root_in_place("),
    ),
    (
        "transfer.rs",
        "transfer_value",
        "Glue::TransferRoot,",
        Through(".transfer_value("),
    ),
];

/// Every `.rs` file under `dir`, with its path relative to `root`.
fn files(root: &Path, dir: &Path, out: &mut Vec<(String, String)>) {
    for entry in std::fs::read_dir(dir).expect("lowering's sources") {
        let path = entry.expect("a directory entry").path();
        if path.is_dir() {
            files(root, &path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            let rel = path.strip_prefix(root).expect("under lower/");
            let rel = rel.to_string_lossy().replace('\\', "/");
            out.push((rel, std::fs::read_to_string(&path).expect("a source file")));
        }
    }
}

/// The name of a function a line declares (`fn name(` or `fn name<`), if it declares one.
fn declared_fn(line: &str) -> Option<&str> {
    let at = line.find("fn ")?;
    let rest = &line[at + 3..];
    let end = rest.find(|c: char| !(c.is_alphanumeric() || c == '_'))?;
    (end > 0 && matches!(rest.as_bytes()[end], b'(' | b'<')).then(|| &rest[..end])
}

#[test]
fn every_site_that_starts_a_transfer_is_listed_with_its_construct() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lower");
    let mut srcs = vec![];
    files(&root, &root, &mut srcs);
    let mut found = BTreeSet::new();
    for (file, src) in &srcs {
        let mut current = "";
        for line in src.lines() {
            if let Some(f) = declared_fn(line) {
                current = f;
            }
            if line.trim_start().starts_with("//") {
                continue;
            }
            for entry in ENTRIES.iter().filter(|e| line.contains(**e)) {
                found.insert((file.clone(), current.to_string(), entry.to_string()));
            }
        }
    }
    let listed: BTreeSet<(String, String, String)> = (SITES.iter())
        .map(|(f, func, e, _)| (f.to_string(), func.to_string(), e.to_string()))
        .collect();
    let new: Vec<_> = found.difference(&listed).collect();
    assert!(
        new.is_empty(),
        "lowering starts a transfer in a place this list doesn't have: make sema mark \
         `Construct::Transfer` for the live construct that reaches it (velt_sema \
         `lazy_std/live_walk.rs`), then list it here:\n{new:#?}"
    );
    let gone: Vec<_> = listed.difference(&found).collect();
    assert!(
        gone.is_empty(),
        "listed sites lowering no longer has:\n{gone:#?}"
    );
}

/// Is a site of `entry` covered by sema other than through `entry` itself (`seen`: the entries
/// on the way)?
fn covered(entry: &str, seen: &mut Vec<&'static str>) -> bool {
    SITES.iter().any(|(_, _, e, cover)| {
        *e == entry
            && match cover {
                By(_) | AsyncClosure => true,
                Through(via) if seen.contains(via) || *via == entry => false,
                Through(via) => {
                    seen.push(via);
                    let c = covered(via, seen);
                    seen.pop();
                    c
                }
            }
    })
}

#[test]
fn each_site_names_what_makes_sema_demand_the_transfer_map() {
    for (file, func, entry, cover) in SITES {
        match cover {
            By(intrinsics) => {
                for &i in *intrinsics {
                    assert!(
                        velt_sema::transfer_starts::intrinsic(i),
                        "{file} `{func}` ({entry}) is covered by {i:?}, which sema does not \
                         mark `Construct::Transfer` for (velt_sema `transfer_starts`)"
                    );
                }
            }
            AsyncClosure => {}
            Through(via) => {
                assert!(
                    ENTRIES.contains(via),
                    "{file} `{func}`: {via} is not an entry"
                );
                assert!(
                    covered(via, &mut vec![]),
                    "{file} `{func}` ({entry}) is reached through {via}, no site of which sema \
                     covers"
                );
            }
        }
    }
}

/// An async closure copying its captures (`AsyncClosure`) is one sema's rule sees: its call is
/// spawned, so it may run on another thread and each call copies what it captured (a closure
/// that stays in its task shares its captures instead, `hir::FnDef::shares_captures`).
const COPIES_CAPTURES: &str = "class N {\n  v = 1;\n}\n\
    async function main() {\n  const n = new N();\n  const go = async () => n.v;\n  \
    console.log(await spawn(go()), n.v);\n}\n";

#[test]
fn an_async_closure_that_copies_its_captures_is_a_transfer_site_sema_sees() {
    if !SITES.iter().any(|s| matches!(s.3, AsyncClosure)) {
        return;
    }
    let p: hir::Program = std_src::check_src(COPIES_CAPTURES);
    let copies = p.defs.iter().any(|d| match d {
        hir::Def::Fn(f) => velt_sema::transfer_starts::copies_captures(f),
        _ => false,
    });
    assert!(
        copies,
        "no async closure copying its captures in:\n{COPIES_CAPTURES}"
    );
}

const SPAWNS_A_GRAPH: &str = "class N {\n  next: N | null = null;\n}\n\
    async function take(n: N): Promise<number> {\n  return n.next === null ? 0 : 1;\n}\n\
    async function main() {\n  const n = new N();\n  const keep = n;\n  \
    console.log(await spawn(take(n)), keep.next === null);\n}\n";

#[test]
fn a_spawn_demands_the_transfer_map() {
    let v = lower_src(SPAWNS_A_GRAPH);
    assert!(
        v.funcs.iter().any(|f| f.symbol.contains("xferBegin")),
        "no transfer begun:\n{v}"
    );
}

#[test]
fn a_program_that_transfers_nothing_loads_no_transfer_map() {
    let v = lower_src("function main() {\n  console.log(\"hi\");\n}\n");
    assert!(
        !v.funcs.iter().any(|f| f.symbol.contains("P8transferN")),
        "the transfer map in a program that transfers nothing:\n{v}"
    );
}

/// A function value whose environment holds a cell (a captured variable that is assigned): its
/// transfer entry copies the cell in a transfer of its own, which nothing in the program starts.
const CLOSURE_TRANSFERS_NOTHING: &str = "class W {\n  seen: number[] = [];\n}\n\
    function main() {\n  const w = new W();\n  let n = 0;\n  \
    const tick = (k: number) => {\n    n += k;\n    w.seen.push(n);\n  };\n  \
    const fs: ((k: number) => void)[] = [tick];\n  fs[0](2);\n  \
    console.log(n, w.seen.length);\n}\n";

#[test]
fn a_closure_in_a_program_that_transfers_nothing_begins_no_transfer() {
    // `check_src` loads std up front (no `demand_std`), so the map's module is in the program
    // though sema demanded none of its hooks: the closure's transfer entry begins nothing (a
    // begin there stopped with an ICE, as `VELT_EAGER_STD=1` builds of two goldens did).
    let p: hir::Program = std_src::check_src(CLOSURE_TRANSFERS_NOTHING);
    let xfer = velt_common::std_hooks::hook_index::xfer_begin;
    assert!(
        !p.hooks_demanded.contains(xfer),
        "sema demanded the transfer map for a program that transfers nothing"
    );
    let v = lower_ok(&p);
    assert!(
        !v.funcs.iter().any(|f| f.symbol.contains("xferBegin")),
        "a transfer begun in a program that transfers nothing:\n{v}"
    );
}
