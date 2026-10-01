//! Embeds the git commit (`git rev-parse --short HEAD`) as `VELT_GIT_HASH` for `velt --version`.
//! Builds outside a git checkout (or without git) get an empty hash instead of failing.

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
