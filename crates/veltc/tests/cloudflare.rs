//! packages/cloudflare's bindings in a `velt build --host host-cloudflare` Worker, under node with
//! fakes of Cloudflare's bindings: tests/cloudflare/drive.mjs imports the build's `worker.mjs` as
//! Workers do, with tests/cloudflare/fakes.mjs's R2 bucket, D1 database, KV namespace, Durable
//! Object namespace and `caches` as its env, and sends the fixture `app/` a request per binding.
//! Debug and release builds; each prints what tests/cloudflare/bindings.out holds: every binding's
//! calls, rejections becoming `IoError`s, and dropped objects releasing their `HostRef`s.
//!
//! Needs what the browser goldens need (tests/wasm_goldens.rs): LLVM's `opt`/`llc`, the
//! wasm32-unknown-unknown Rust target and node; skipped with a note when one is missing.

mod no_window;
mod wasm_tools;
mod work_dir;

use std::path::{Path, PathBuf};

use wasm_tools::{build_runtime, copy_dir, for_node, missing_tools, root};

const TRIPLE: &str = "wasm32-unknown-unknown";

/// The fixtures in a work directory of their own, with the packages they depend on beside them.
fn fixtures(name: &str) -> Option<PathBuf> {
    if let Some(missing) = missing_tools(TRIPLE, "node") {
        eprintln!("note: {missing} not available; skipping the Cloudflare bindings test");
        return None;
    }
    let root = root();
    build_runtime(&root, TRIPLE);
    let work =
        work_dir::work_dir(&root, "cloudflare-work").join(format!("{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    copy_dir(&root.join("crates/veltc/tests/cloudflare"), &work);
    for package in ["cloudflare", "host-cloudflare"] {
        copy_dir(&root.join("packages").join(package), &work.join(package));
    }
    Some(work)
}

fn velt(dir: &Path, args: &[&str]) -> std::process::Output {
    no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run velt")
}

/// Every binding against the fakes, in a debug and a release build.
#[test]
fn bindings_work_against_fakes() {
    let Some(work) = fixtures("bindings") else {
        return;
    };
    let dir = work.join("app");
    let want = std::fs::read_to_string(work.join("bindings.out"))
        .expect("expected output")
        .replace("\r\n", "\n");
    for mode in [None, Some("--release")] {
        let args: Vec<&str> = ["build", "--host", "host-cloudflare"]
            .into_iter()
            .chain(mode)
            .collect();
        let o = velt(&dir, &args);
        assert!(
            o.status.success(),
            "velt {args:?}:\n{}",
            String::from_utf8_lossy(&o.stderr)
        );
        let worker = dir.join("target/velt/app-workers");
        assert!(worker.join("hosts/cloudflare/cloudflare.mjs").is_file());
        let o = no_window::command("node")
            .arg(for_node(&work.join("drive.mjs")))
            .arg(for_node(&worker))
            .current_dir(&dir)
            .output()
            .expect("run node");
        let got = String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n");
        assert!(
            o.status.success() && got == want,
            "{args:?}: exit {:?}\n--- want ---\n{want}--- got ---\n{got}--- stderr ---\n{}",
            o.status.code(),
            String::from_utf8_lossy(&o.stderr)
        );
    }
    let _ = std::fs::remove_dir_all(&work);
}

/// The bindings call host functions, which only `--host` builds have: a native build of a
/// program calling one is an error, while the package itself checks natively.
#[test]
fn native_builds_reject_calls_and_check_the_package() {
    let Some(work) = fixtures("native") else {
        return;
    };
    let o = velt(&work.join("cloudflare"), &["check"]);
    assert!(
        o.status.success(),
        "velt check in packages/cloudflare:\n{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let o = velt(&work.join("app"), &["build"]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        !o.status.success() && err.contains("host function"),
        "a native build of app/:\n{err}"
    );
    let _ = std::fs::remove_dir_all(&work);
}
