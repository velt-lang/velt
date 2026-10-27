//! The goldens' build in one file, so CI builds once per OS and checks the goldens in parallel
//! shards on other runners: `cargo xtask check --part golden --pack <file>` builds and packs,
//! `cargo xtask check --part golden --archive <file>` unpacks and runs them without building
//! (with `VELT_GOLDEN_SHARD=<i>/<n>`, one shard). `--part wasm` (or `golden,wasm`) does the same
//! for the WebAssembly goldens (`VELT_WASM_SHARD=<i>/<n>`).
//!
//! The archive is a tar of what the goldens run, at its path under the checkout: `velt`,
//! `velt-kit`, the runtime libraries next to them, veltc's golden test binary, and
//! [`MANIFEST`], which names the test binaries and the checkout they were built in; with the
//! WebAssembly goldens also their test binary and the WebAssembly runtime libraries
//! ([`WASM_RUNTIMES`]). A test binary has that checkout's absolute paths compiled in
//! (`CARGO_MANIFEST_DIR`, `CARGO_BIN_EXE_velt`), so it unpacks only into a checkout at the same
//! path (each CI runner of an OS has its workspace at the same path). Cargo's dep-info files
//! stay out: the runtime's freshness check (crates/veltc/tests/runtime_support) compares them
//! with the sources' modification times, which a fresh checkout sets to the time of the
//! checkout. The WebAssembly goldens build their runtime with cargo unless
//! `VELT_RT_WASM_PREBUILT=1`, which the run from an archive sets.

use std::path::Path;

use crate::check::exe;

/// The manifest's path in the archive and the checkout.
pub const MANIFEST: &str = "target/golden-archive.txt";

/// What [`MANIFEST`] says.
#[derive(Debug, PartialEq, Eq)]
pub struct Manifest {
    /// The checkout the archive was built in.
    pub root: String,
    /// The golden test binary, relative to the checkout, `/`-separated, if packed.
    pub test: Option<String>,
    /// The WebAssembly goldens' test binary, likewise.
    pub wasm: Option<String>,
}

impl Manifest {
    pub fn render(&self) -> String {
        let mut text = format!("root={}\n", self.root);
        for (name, value) in [("test", &self.test), ("wasm", &self.wasm)] {
            if let Some(value) = value {
                text.push_str(&format!("{name}={value}\n"));
            }
        }
        text
    }

    pub fn parse(text: &str) -> Result<Manifest, String> {
        let field = |name: &str| {
            text.lines()
                .find_map(|l| l.strip_prefix(name)?.strip_prefix('='))
                .map(str::to_string)
        };
        let manifest = Manifest {
            root: field("root").ok_or(format!("{MANIFEST} has no `root=` line"))?,
            test: field("test"),
            wasm: field("wasm"),
        };
        if manifest.test.is_none() && manifest.wasm.is_none() {
            return Err(format!("{MANIFEST} names no test binary"));
        }
        Ok(manifest)
    }
}

/// The targets the WebAssembly goldens build for, and the runtime library each links, relative
/// to the checkout.
pub const WASM_RUNTIMES: &[(&str, &str)] = &[
    (
        "wasm32-wasip1",
        "target/wasm32-wasip1/debug/libvelt_rt_wasm.a",
    ),
    (
        "wasm32-unknown-unknown",
        "target/wasm32-unknown-unknown/debug/libvelt_rt_wasm.a",
    ),
];

/// The files besides the test binary and the manifest, relative to the checkout: the `velt`
/// and `velt-kit` binaries and the runtime libraries of the host, those of `existing` that
/// exist (musl has no shared runtime).
pub fn files(existing: impl Fn(&str) -> bool) -> Vec<String> {
    let mut names = vec![exe("velt"), exe("velt-kit")];
    let libs: &[&str] = if cfg!(windows) {
        &[
            "velt_rt.lib",
            "velt_rt_shared.dll",
            "velt_rt_shared.dll.lib",
        ]
    } else if cfg!(target_os = "macos") {
        &["libvelt_rt.a", "libvelt_rt_shared.dylib"]
    } else {
        &["libvelt_rt.a", "libvelt_rt_shared.so"]
    };
    names.extend(libs.iter().map(|s| s.to_string()));
    names
        .into_iter()
        .map(|n| format!("target/debug/{n}"))
        .filter(|p| existing(p))
        .collect()
}

/// veltc's integration test binary `name` (`golden`, `wasm_goldens`) in the JSON messages of
/// `cargo test --no-run --message-format=json` (velt_syntax has a test named `golden` too).
pub fn test_binary(messages: &str, name: &str) -> Option<String> {
    let file = format!("crates/veltc/tests/{name}.rs");
    messages.lines().find_map(|line| {
        let src = json_string(line, "src_path")?.replace('\\', "/");
        let is_test = line.contains(r#""kind":["test"]"#);
        (is_test && src.ends_with(&file))
            .then(|| json_string(line, "executable"))
            .flatten()
    })
}

/// `VELT_WASM_SHARD`'s `<i>/<n>` (1 <= i <= n).
pub fn parse_shard(value: &str) -> Result<(usize, usize), String> {
    let bad = || format!("VELT_WASM_SHARD={value}: expected <i>/<n> with 1 <= i <= n");
    let (i, n) = value.split_once('/').ok_or_else(bad)?;
    let i: usize = i.trim().parse().map_err(|_| bad())?;
    let n: usize = n.trim().parse().map_err(|_| bad())?;
    if i == 0 || i > n {
        return Err(bad());
    }
    Ok((i, n))
}

/// The test names in the output of a libtest binary's `--list --format terse`.
pub fn listed_tests(list: &str) -> Vec<String> {
    list.lines()
        .filter_map(|l| l.trim().strip_suffix(": test"))
        .map(str::to_string)
        .collect()
}

/// The tests of shard `i` of `n`: every `n`-th one, the long ones (the wasm32-wasip1 shard
/// functions, `…_shard_<k>`, in the order of `k`) dealt out first, so that no shard gets more
/// than one of them more than another.
pub fn shard_tests(tests: &[String], i: usize, n: usize) -> Vec<String> {
    let long_index = |t: &String| {
        t.rsplit_once("_shard_")
            .and_then(|(_, k)| k.parse::<usize>().ok())
    };
    let mut long: Vec<(usize, &String)> = tests
        .iter()
        .filter_map(|t| long_index(t).map(|k| (k, t)))
        .collect();
    long.sort();
    let mut rest: Vec<&String> = tests.iter().filter(|t| long_index(t).is_none()).collect();
    rest.sort();
    long.into_iter()
        .map(|(_, t)| t)
        .chain(rest)
        .enumerate()
        .filter(|(k, _)| k % n == i - 1)
        .map(|(_, t)| t.clone())
        .collect()
}

/// The value of the string field `key` in a JSON line (the first one; cargo's messages have no
/// nested field of these names before the one meant).
fn json_string(line: &str, key: &str) -> Option<String> {
    let start = line.find(&format!("\"{key}\":\""))? + key.len() + 4;
    let mut out = String::new();
    let mut chars = line[start..].chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                c => out.push(c),
            },
            c => out.push(c),
        }
    }
    None
}

/// `path` relative to `root`, `/`-separated.
pub fn relative(root: &Path, path: &str) -> Result<String, String> {
    Path::new(path)
        .strip_prefix(root)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .map_err(|_| format!("{path} is not inside {}", root.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_manifest_round_trips() {
        let mut m = Manifest {
            root: r"D:\a\velt\velt".into(),
            test: Some("target/debug/deps/golden-0123abcd.exe".into()),
            wasm: None,
        };
        assert_eq!(Manifest::parse(&m.render()).unwrap(), m);
        m.wasm = Some("target/debug/deps/wasm_goldens-4567.exe".into());
        assert_eq!(Manifest::parse(&m.render()).unwrap(), m);
        m.test = None;
        assert_eq!(Manifest::parse(&m.render()).unwrap(), m);
        assert!(Manifest::parse("root=/x\n").is_err());
        assert!(Manifest::parse("test=/x\n").is_err());
    }

    #[test]
    fn shards_are_parsed_strictly() {
        assert_eq!(parse_shard("2/4"), Ok((2, 4)));
        assert_eq!(parse_shard("1/1"), Ok((1, 1)));
        for bad in ["0/4", "5/4", "4", "a/4", "1/", ""] {
            assert!(parse_shard(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_listed_tests_are_read_from_libtests_terse_list() {
        let list = "goldens_in_the_browser_glue: test\ngoldens_under_wasmtime_shard_0: test\n\n2 tests, 0 benchmarks\n";
        assert_eq!(
            listed_tests(list),
            [
                "goldens_in_the_browser_glue",
                "goldens_under_wasmtime_shard_0"
            ]
        );
    }

    #[test]
    fn the_shards_deal_out_the_long_tests_first_and_cover_every_test_once() {
        let mut tests: Vec<String> = (0..8)
            .map(|k| format!("goldens_under_wasmtime_shard_{k}"))
            .collect();
        tests.push("goldens_in_the_browser_glue".into());
        tests.push("wasmtime_shards_cover_every_golden_once".into());
        tests.reverse();
        let long = |shard: &[String]| shard.iter().filter(|t| t.contains("_shard_")).count();
        let mut want = tests.clone();
        want.sort();
        for n in 1..=12 {
            let shards: Vec<Vec<String>> = (1..=n).map(|i| shard_tests(&tests, i, n)).collect();
            let mut all: Vec<String> = shards.concat();
            all.sort();
            assert_eq!(all, want, "{n} shards");
            let most = shards.iter().map(|s| long(s)).max().unwrap_or(0);
            let least = shards.iter().map(|s| long(s)).min().unwrap_or(0);
            assert!(most - least <= 1, "{n} shards: {shards:?}");
        }
        assert_eq!(
            shard_tests(&tests, 1, 4),
            [
                "goldens_under_wasmtime_shard_0",
                "goldens_under_wasmtime_shard_4",
                "goldens_in_the_browser_glue"
            ]
        );
    }

    #[test]
    fn the_test_binary_is_veltcs_golden_test() {
        let syntax = r#"{"reason":"compiler-artifact","target":{"kind":["test"],"name":"golden","src_path":"/w/crates/velt_syntax/tests/golden.rs"},"executable":"/w/target/debug/deps/golden-aaaa"}"#;
        let lib = r#"{"reason":"compiler-artifact","target":{"kind":["lib"],"name":"veltc","src_path":"/w/crates/veltc/tests/golden.rs"},"executable":null}"#;
        let veltc = r#"{"reason":"compiler-artifact","target":{"kind":["test"],"name":"golden","src_path":"/w/crates/veltc/tests/golden.rs"},"executable":"/w/target/debug/deps/golden-bbbb"}"#;
        let messages = [syntax, lib, veltc].join("\n");
        assert_eq!(
            test_binary(&messages, "golden").as_deref(),
            Some("/w/target/debug/deps/golden-bbbb")
        );
        assert_eq!(test_binary(syntax, "golden"), None);
        let wasm = r#"{"reason":"compiler-artifact","target":{"kind":["test"],"name":"wasm_goldens","src_path":"/w/crates/veltc/tests/wasm_goldens.rs"},"executable":"/w/target/debug/deps/wasm_goldens-cccc"}"#;
        let messages = [syntax, veltc, wasm].join("\n");
        assert_eq!(
            test_binary(&messages, "wasm_goldens").as_deref(),
            Some("/w/target/debug/deps/wasm_goldens-cccc")
        );
        assert_eq!(test_binary(veltc, "wasm_goldens"), None);
    }

    #[test]
    fn windows_paths_are_unescaped() {
        let line = r#"{"target":{"kind":["test"],"src_path":"D:\\a\\v\\crates\\veltc\\tests\\golden.rs"},"executable":"D:\\a\\v\\target\\debug\\deps\\golden-c.exe"}"#;
        assert_eq!(
            test_binary(line, "golden").as_deref(),
            Some(r"D:\a\v\target\debug\deps\golden-c.exe")
        );
    }

    #[test]
    fn files_are_under_target_debug() {
        let all = files(|_| true);
        assert!(all.iter().all(|f| f.starts_with("target/debug/")));
        assert!(all.contains(&format!("target/debug/{}", exe("velt"))));
        assert!(files(|f| !f.contains("shared")).len() < all.len());
    }

    #[test]
    fn paths_are_made_relative_to_the_checkout() {
        let root = std::env::temp_dir().join("checkout");
        let exe = root.join("target").join("debug").join("velt");
        assert_eq!(
            relative(&root, &exe.to_string_lossy()).unwrap(),
            "target/debug/velt"
        );
        assert!(relative(&root, "/elsewhere/velt").is_err());
    }
}
