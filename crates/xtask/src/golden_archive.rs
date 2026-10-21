//! The goldens' build in one file, so CI builds once per OS and checks the goldens in parallel
//! shards on other runners: `cargo xtask check --part golden --pack <file>` builds and packs,
//! `cargo xtask check --part golden --archive <file>` unpacks and runs them without building
//! (with `VELT_GOLDEN_SHARD=<i>/<n>`, one shard).
//!
//! The archive is a tar of what the goldens run, at its path under the checkout: `velt`,
//! `velt-kit`, the runtime libraries next to them, veltc's golden test binary, and
//! [`MANIFEST`], which names the test binary and the checkout it was built in. The test binary
//! has that checkout's absolute paths compiled in (`CARGO_MANIFEST_DIR`, `CARGO_BIN_EXE_velt`),
//! so it unpacks only into a checkout at the same path (each CI runner of an OS has its
//! workspace at the same path). Cargo's dep-info files stay out: the runtime's freshness check
//! (crates/veltc/tests/runtime_support) compares them with the sources' modification times,
//! which a fresh checkout sets to the time of the checkout.

use std::path::Path;

use crate::check::exe;

/// The manifest's path in the archive and the checkout.
pub const MANIFEST: &str = "target/golden-archive.txt";

/// What [`MANIFEST`] says.
#[derive(Debug, PartialEq, Eq)]
pub struct Manifest {
    /// The checkout the archive was built in.
    pub root: String,
    /// The golden test binary, relative to the checkout, `/`-separated.
    pub test: String,
}

impl Manifest {
    pub fn render(&self) -> String {
        format!("root={}\ntest={}\n", self.root, self.test)
    }

    pub fn parse(text: &str) -> Result<Manifest, String> {
        let field = |name: &str| {
            text.lines()
                .find_map(|l| l.strip_prefix(name)?.strip_prefix('='))
                .map(str::to_string)
                .ok_or(format!("{MANIFEST} has no `{name}=` line"))
        };
        Ok(Manifest {
            root: field("root")?,
            test: field("test")?,
        })
    }
}

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

/// veltc's golden test binary in the JSON messages of `cargo test --no-run
/// --message-format=json` (velt_syntax has a test named `golden` too).
pub fn test_binary(messages: &str) -> Option<String> {
    messages.lines().find_map(|line| {
        let src = json_string(line, "src_path")?.replace('\\', "/");
        let is_test = line.contains(r#""kind":["test"]"#);
        (is_test && src.ends_with("crates/veltc/tests/golden.rs"))
            .then(|| json_string(line, "executable"))
            .flatten()
    })
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
        let m = Manifest {
            root: r"D:\a\velt\velt".into(),
            test: "target/debug/deps/golden-0123abcd.exe".into(),
        };
        assert_eq!(Manifest::parse(&m.render()).unwrap(), m);
        assert!(Manifest::parse("root=/x\n").is_err());
    }

    #[test]
    fn the_test_binary_is_veltcs_golden_test() {
        let syntax = r#"{"reason":"compiler-artifact","target":{"kind":["test"],"name":"golden","src_path":"/w/crates/velt_syntax/tests/golden.rs"},"executable":"/w/target/debug/deps/golden-aaaa"}"#;
        let lib = r#"{"reason":"compiler-artifact","target":{"kind":["lib"],"name":"veltc","src_path":"/w/crates/veltc/tests/golden.rs"},"executable":null}"#;
        let veltc = r#"{"reason":"compiler-artifact","target":{"kind":["test"],"name":"golden","src_path":"/w/crates/veltc/tests/golden.rs"},"executable":"/w/target/debug/deps/golden-bbbb"}"#;
        let messages = [syntax, lib, veltc].join("\n");
        assert_eq!(
            test_binary(&messages).as_deref(),
            Some("/w/target/debug/deps/golden-bbbb")
        );
        assert_eq!(test_binary(syntax), None);
    }

    #[test]
    fn windows_paths_are_unescaped() {
        let line = r#"{"target":{"kind":["test"],"src_path":"D:\\a\\v\\crates\\veltc\\tests\\golden.rs"},"executable":"D:\\a\\v\\target\\debug\\deps\\golden-c.exe"}"#;
        assert_eq!(
            test_binary(line).as_deref(),
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
