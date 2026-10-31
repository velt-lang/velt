//! Embeds the git commit (`git rev-parse --short HEAD`) as `VELT_GIT_HASH` for `velt --version`.
//! Builds outside a git checkout (or without git) get an empty hash instead of failing.
//!
//! `VELT_RELEASE_TAG` is `v<version>` when this is the release of that version (the commit has
//! the tag, or GitHub Actions builds it for that tag push), else empty: `velt new` pins the
//! packages it takes from the Velt repository to the tag (crates/veltc/src/templates.rs).

use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    let text = String::from_utf8(out.stdout).ok()?;
    let text = text.trim();
    (out.status.success() && !text.is_empty()).then(|| text.to_string())
}

fn main() {
    let hash = git(&["rev-parse", "--short", "HEAD"]).unwrap_or_default();
    println!("cargo:rustc-env=VELT_GIT_HASH={hash}");
    let tag = format!(
        "v{}",
        std::env::var("CARGO_PKG_VERSION").unwrap_or_default()
    );
    let pushed = std::env::var("GITHUB_REF_TYPE").is_ok_and(|t| t == "tag")
        && std::env::var("GITHUB_REF_NAME").is_ok_and(|n| n == tag);
    let tagged = git(&["tag", "--points-at", "HEAD"]).is_some_and(|t| t.lines().any(|l| l == tag));
    let release = if pushed || tagged { tag.as_str() } else { "" };
    println!("cargo:rustc-env=VELT_RELEASE_TAG={release}");
    println!("cargo:rerun-if-env-changed=GITHUB_REF_TYPE");
    println!("cargo:rerun-if-env-changed=GITHUB_REF_NAME");
    // Re-run when HEAD moves: HEAD itself (branch switches), the branch ref, and packed refs.
    let mut watched = vec!["HEAD".to_string(), "packed-refs".to_string()];
    watched.extend(git(&["symbolic-ref", "-q", "HEAD"]));
    for rel in watched {
        // A missing path would make cargo re-run this script on every build.
        let path = git(&["rev-parse", "--path-format=absolute", "--git-path", &rel]);
        if let Some(path) = path.filter(|p| std::path::Path::new(p).exists()) {
            println!("cargo:rerun-if-changed={path}");
        }
    }
    println!("cargo:rerun-if-changed=build.rs");
}
