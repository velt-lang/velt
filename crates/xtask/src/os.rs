//! Whether a change needs the merge queue to check Windows and macOS too, or Linux is enough.
//!
//! The OS-specific code is in the runtime, the native crates, code generation and linking, the
//! `velt` command line and packages, and the standard library; a change there (or to the build,
//! CI or scripts, which select everything) runs every OS in the queue. The front end and the
//! tooling are portable Rust: Linux checks them, and the full gate on Windows and macOS after
//! the push to `main` catches the rare exception. A change the queue checked on every OS skips
//! that second run ([`after_merge_reason`]).

use crate::graph::Graph;

/// Crates without OS-specific code. A crate not listed here (a new one too) counts as
/// OS-specific.
pub(crate) const PORTABLE_CRATES: &[&str] = &[
    "velt_common",
    "velt_syntax",
    "velt_sema",
    "velt_vir",
    "velt_opt",
    "velt_fmt",
    "velt_lsp",
    "velt_doc",
    "velt_tscompat",
    "velt_registry",
    "velt_http",
    "velt_rt_wasm",
];

/// Paths outside `crates/` whose behavior can differ by OS: the standard library's bindings,
/// end-to-end programs and their expected output, native packages.
const OS_SPECIFIC_PATHS: &[&str] = &["std/", "tests/golden/", "examples/", "packages/"];

/// Why Windows and macOS must be checked too, or `None` when Linux is enough. `full` is
/// whether the paths select everything (build, CI and script changes).
pub fn other_os_reason(graph: &Graph, paths: &[String], full: bool) -> Option<String> {
    if full {
        return Some("the change selects every check".into());
    }
    for path in paths {
        if let Some(rest) = path.strip_prefix("crates/") {
            let dir = rest.split('/').next().unwrap_or(rest);
            match graph.dirs.get(dir) {
                Some(name) if PORTABLE_CRATES.contains(&name.as_str()) => {}
                Some(name) => return Some(format!("{name} has OS-specific code")),
                None => return Some(format!("{path}: unknown crate")),
            }
        } else if OS_SPECIFIC_PATHS.iter().any(|p| path.starts_with(p)) {
            return Some(format!("{path} can behave differently by OS"));
        }
    }
    None
}

/// Files that key or fill the build cache (Swatinem/rust-cache in `.github/actions/setup`): the
/// toolchain and the dependencies. Only the pushes to `main` save the cache. A dependency change
/// reaches `Cargo.lock`. Once #1135 C5 keys the cache by `rust-toolchain.toml` and `Cargo.lock`'s
/// external packages only, an edit to a `Cargo.toml` alone changes nothing the cache holds; until
/// then such an edit changes the key without refreshing the Windows and macOS caches.
const CACHE_INPUTS: &[&str] = &[
    "Cargo.lock",
    "rust-toolchain.toml",
    ".cargo/",
    ".github/actions/setup/",
];

/// Why the whole gate runs on Windows and macOS after a change lands on `main`, or `None` when
/// the merge queue already ran it there. `other_os` is the change's [`other_os_reason`]: on the
/// push, `cargo xtask affected --base HEAD^1` plans the same change the queue planned (the queue
/// tests each entry on its `base_sha`, which becomes `HEAD^1` on `main`). The gate runs when
///
/// - the queue checked Linux only, so Windows and macOS have not seen the change, or
/// - the change touches the toolchain or the dependencies: the run refreshes the Windows and
///   macOS build caches, which only pushes to `main` save.
pub fn after_merge_reason(paths: &[String], other_os: &Option<String>) -> Option<String> {
    if other_os.is_none() {
        return Some("the merge queue checked Linux only".into());
    }
    paths
        .iter()
        .find(|p| CACHE_INPUTS.iter().any(|c| p.starts_with(c)))
        .map(|p| format!("{p} changed: refresh the build cache"))
}

/// The runners the merge queue checks on: Linux always, the others for OS-specific changes.
pub const QUEUE_OSES: &[&str] = &["ubuntu-latest", "windows-latest", "macos-latest"];

/// How many jobs check the goldens in parallel on each OS in the merge queue
/// (.github/workflows/queue-goldens.yml), after one job built them. Windows, whose goldens take
/// longest, gets the most. macOS gets two: in one job its goldens grew to the job's 30-minute
/// limit (2026-10). With 5 macOS runners for everything, more shards would let queue entries
/// starve each other.
/// Every runner in [`QUEUE_OSES`] needs an entry: the workflow fails when an OS has no shards.
pub const GOLDEN_SHARDS: &[(&str, usize)] = &[
    ("ubuntu-latest", 2),
    ("windows-latest", 3),
    ("macos-latest", 2),
];

/// How many jobs check the WebAssembly goldens (crates/veltc/tests/wasm_goldens.rs) in parallel
/// on each OS, in the merge queue when a change touches WebAssembly code and in the nightly, from
/// the build of the goldens' job (.github/workflows/queue-goldens.yml). They take about as long
/// as all the other tests together: on a 4-vCPU Windows runner about 45 minutes in one job. The
/// test splits them into 8 functions of about 330 s there, which the shards share (two each on
/// Windows). Every runner in [`QUEUE_OSES`] needs an entry, as for [`GOLDEN_SHARDS`].
pub const WASM_SHARDS: &[(&str, usize)] = &[
    ("ubuntu-latest", 2),
    ("windows-latest", 4),
    ("macos-latest", 2),
];

/// The queue's golden shards (`VELT_GOLDEN_SHARD` values) on each of the runners `oses`, as a
/// JSON object: `{"ubuntu-latest":["1/2","2/2"],…}`. A runner without a [`GOLDEN_SHARDS`] entry
/// gets none, which fails the queue (never a silent skip of its goldens).
pub fn golden_shards_json(oses: &[&str]) -> String {
    shards_json(GOLDEN_SHARDS, oses)
}

/// The WebAssembly goldens' shards (`VELT_WASM_SHARD` values) on each of the runners `oses`, as
/// [`golden_shards_json`] gives the goldens'.
pub fn wasm_shards_json(oses: &[&str]) -> String {
    shards_json(WASM_SHARDS, oses)
}

fn shards_json(table: &[(&str, usize)], oses: &[&str]) -> String {
    let mut entries = vec![];
    for os in oses {
        let Some(&(_, n)) = table.iter().find(|(name, _)| name == os) else {
            continue;
        };
        let shards: Vec<String> = (1..=n).map(|i| format!("\"{i}/{n}\"")).collect();
        entries.push(format!("\"{os}\":[{}]", shards.join(",")));
    }
    format!("{{{}}}", entries.join(","))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_os_gets_its_shards() {
        assert_eq!(
            golden_shards_json(&["ubuntu-latest"]),
            r#"{"ubuntu-latest":["1/2","2/2"]}"#
        );
        let all = golden_shards_json(QUEUE_OSES);
        let total: usize = GOLDEN_SHARDS.iter().map(|(_, n)| n).sum();
        assert_eq!(all.matches('/').count(), total);
        assert!(
            all.contains(r#""windows-latest":["1/3","2/3","3/3"]"#),
            "{all}"
        );
        assert!(all.contains(r#""macos-latest":["1/2","2/2"]"#), "{all}");
        assert_eq!(golden_shards_json(&["other"]), "{}");
        assert_eq!(golden_shards_json(&[]), "{}");
    }

    #[test]
    fn every_queue_os_has_at_least_one_golden_shard() {
        for table in [GOLDEN_SHARDS, WASM_SHARDS] {
            for os in QUEUE_OSES {
                let n = table.iter().find(|(name, _)| name == os).map(|e| e.1);
                assert!(n.is_some_and(|n| n >= 1), "{os} has no shards");
            }
        }
    }

    #[test]
    fn the_webassembly_goldens_get_shards_of_their_own() {
        let all = wasm_shards_json(QUEUE_OSES);
        assert!(
            all.contains(r#""windows-latest":["1/4","2/4","3/4","4/4"]"#),
            "{all}"
        );
        assert!(all.contains(r#""macos-latest":["1/2","2/2"]"#), "{all}");
        assert_eq!(
            wasm_shards_json(&["ubuntu-latest"]),
            r#"{"ubuntu-latest":["1/2","2/2"]}"#
        );
        assert_eq!(wasm_shards_json(&["other"]), "{}");
    }
}
