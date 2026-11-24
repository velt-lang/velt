//! Where lowering can start a transfer to another thread (std's transfer map, #3584): each call
//! of a transfer-starting entry point in lowering's sources, by the function it is in, with the
//! live construct sema marks `Construct::Transfer` for (velt_sema `lazy_std/live_walk.rs`) or the
//! entry point that already covers it. A begin where sema did not demand the construct is an
//! ICE (lower/transfer_map.rs), but only in a program that reaches it: this list makes a new site
//! fail here, in every build, so its construct is marked before a program no golden covers
//! reaches it. A site that is gone fails too, so the list stays the whole of them.

use std::collections::BTreeSet;
use std::path::Path;

use velt_sema::hir;

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

/// `(file, function, entry)` for every call of an entry, with what makes sema demand
/// `Construct::Transfer` wherever it runs.
const SITES: &[(&str, &str, &str, &str)] = &[
    ("adt.rs", "shared_new", ".many_walk(", "SharedNew"),
    ("adt.rs", "shared_new", ".transfer_value(", "SharedNew"),
    (
        "async_fn/ctor.rs",
        "state_from_closure",
        ".maybe_transfer(",
        "Spawn (transfer_args)",
    ),
    (
        "async_fn/ctor.rs",
        "take_capture",
        ".clone_keeping_identity(",
        "an async closure that copies its captures",
    ),
    (
        "async_fn/spawn.rs",
        "spawn",
        "Glue::TransferRoot,",
        "Spawn, SpawnHandled",
    ),
    (
        "async_fn/spawn.rs",
        "transfer_result",
        "Glue::TransferRoot,",
        "Spawn (transfer_call)",
    ),
    (
        "async_fn/suspend.rs",
        "async_args",
        ".maybe_transfer(",
        "Spawn (transfer_args)",
    ),
    (
        "async_fn/sync.rs",
        "leave_lock",
        ".reader_keep(",
        "MutexWith, LockRead, SharedRead",
    ),
    (
        "async_fn/sync.rs",
        "leave_lock",
        ".transfer_value(",
        "MutexWith, LockRead, SharedRead",
    ),
    (
        "async_fn/sync.rs",
        "mutex_with",
        ".many_walk(",
        "MutexWith, LockRead",
    ),
    (
        "async_fn/sync.rs",
        "mutex_with",
        ".transfer_root_in_place(",
        "MutexWith, LockRead",
    ),
    (
        "call.rs",
        "borrow_args",
        ".transfer_copy(",
        "Spawn (transfer_call)",
    ),
    (
        "callee.rs",
        "call_indirect_in",
        ".transfer_value(",
        "Spawn (transfer_call)",
    ),
    (
        "caller_stores.rs",
        "copy_mut_arg",
        ".reader_copy(",
        "LockRead, SharedRead (read mode)",
    ),
    (
        "caller_stores.rs",
        "count_for_caller",
        ".reader_keep(",
        "LockRead, SharedRead (read mode)",
    ),
    (
        "caller_stores.rs",
        "keep_mut_arg",
        ".reader_keep(",
        "LockRead, SharedRead (read mode)",
    ),
    (
        "caller_stores.rs",
        "keep_value",
        ".reader_copy(",
        "LockRead, SharedRead (read mode)",
    ),
    (
        "caller_stores.rs",
        "keep_value",
        ".reader_keep(",
        "LockRead, SharedRead (read mode)",
    ),
    (
        "caller_stores.rs",
        "reader_keep",
        ".xfer_keep_begin()",
        "its callers",
    ),
    (
        "caller_stores.rs",
        "reader_keep",
        "Glue::TransferRoot,",
        "its callers",
    ),
    (
        "closure.rs",
        "fill_env_from",
        ".maybe_transfer(",
        "Spawn (transfer_args)",
    ),
    (
        "dispatch.rs",
        "receiver",
        ".transfer_copy(",
        "Spawn (transfer_call)",
    ),
    ("glue/many.rs", "many_walk", ".xfer_begin()", "its callers"),
    (
        "glue/transfer.rs",
        "transfer_promise",
        "Glue::TransferRoot,",
        "the transfer glue",
    ),
    (
        "glue/transfer.rs",
        "transfer_root_body",
        ".xfer_begin()",
        "the TransferRoot glue",
    ),
    (
        "glue/transfer_env.rs",
        "answer_call_copy",
        ".xfer_begin()",
        "SharedNew (boxing.shared_fns)",
    ),
    (
        "glue/transfer_env.rs",
        "own_cell",
        ".thread_copy(",
        "the transfer glue",
    ),
    (
        "intrinsics.rs",
        "intrinsic",
        ".transfer_stored(",
        "Transfer",
    ),
    (
        "leave_levels.rs",
        "share_stored",
        ".transfer_value(",
        "its callers",
    ),
    (
        "leave_levels.rs",
        "share_stored",
        ".xfer_share_begin()",
        "its callers",
    ),
    (
        "leave_levels.rs",
        "transfer_stored",
        ".share_stored(",
        "Transfer",
    ),
    ("slot.rs", "slot_put", ".transfer_value(", "SlotPut"),
    (
        "slot.rs",
        "slot_transfer",
        ".transfer_value(",
        "SlotTransfer",
    ),
    (
        "transfer.rs",
        "clone_keeping_identity",
        ".xfer_begin()",
        "its callers",
    ),
    (
        "transfer.rs",
        "maybe_transfer",
        ".transfer_value(",
        "its callers",
    ),
    (
        "transfer.rs",
        "reader_copy",
        "Glue::TransferRoot,",
        "its callers",
    ),
    ("transfer.rs", "thread_copy", ".xfer_begin()", "its callers"),
    (
        "transfer.rs",
        "transfer_copy",
        ".thread_copy(",
        "its callers",
    ),
    (
        "transfer.rs",
        "transfer_root_in_place",
        "Glue::TransferRoot,",
        "its callers",
    ),
    (
        "transfer.rs",
        "transfer_value",
        "Glue::TransferRoot,",
        "its callers",
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

#[test]
#[should_panic(expected = "ICE: hook xfer_begin reached by lowering but not demanded by sema")]
fn a_transfer_sema_did_not_demand_is_an_ice() {
    let p: hir::Program = std_src::check_src(SPAWNS_A_GRAPH);
    // Every hook sema demanded but the transfer map's.
    let mut demanded = velt_common::std_hooks::HookSet::default();
    for (i, h) in velt_common::std_hooks::HOOKS.iter().enumerate() {
        let xfer = h
            .constructs
            .contains(&velt_common::std_hooks::Construct::Transfer);
        if p.hooks_demanded.contains(i) && !xfer {
            demanded.insert(i);
        }
    }
    let p = hir::Program {
        hooks_demanded: demanded,
        ..p
    };
    lower_ok(&p);
}
