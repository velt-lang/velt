//! A package with native code end to end (docs/internals/design/native-packages.md), with the
//! small package `tests/native_package/`:
//!
//! 1. the author builds the library (`velt native build`, cargo) and publishes the package with
//!    it to a `velt registry serve`;
//! 2. a user **without cargo** adds the package: the prebuilt library is downloaded, verified
//!    and listed as native code;
//! 3. `velt run` (debug: the shared library), `velt build --release` (static: the executable
//!    still runs after the cache is deleted) and `velt dev` (loaded into the JIT host; a Velt
//!    edit still hot-swaps, and what the library held before the swap is still there).
//!
//! Without cargo it fails, unless `VELT_SKIP_NATIVE_E2E=1`.

mod no_window;
#[allow(dead_code)]
mod reload_support;
mod runtime_support;
mod test_dir;

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Output, Stdio};

use reload_support::{get, Dev, Mark};

const NO_CARGO: &str = "velt-test-no-such-cargo";

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn velt(dir: &Path, home: &Path, args: &[&str]) -> Output {
    crate::no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(args)
        .current_dir(dir)
        .env("VELT_HOME", home)
        .env("VELT_CARGO", NO_CARGO)
        .env("VELT_REGISTRY", home.join("registry"))
        .env_remove("VELT_REGISTRY_TOKEN")
        .output()
        .expect("run velt")
}

fn ok(out: Output, what: &str) -> Output {
    assert!(
        out.status.success(),
        "{what} failed:\n--- stdout ---\n{}\n--- stderr ---\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

/// Kills the registry server when the test ends.
struct Kill(Child);

impl Drop for Kill {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap().flatten() {
        let name = e.file_name();
        if name == "target" {
            continue;
        }
        let (src, dst) = (e.path(), to.join(&name));
        if src.is_dir() {
            copy_dir(&src, &dst);
        } else {
            std::fs::copy(&src, &dst).unwrap();
        }
    }
}

const PROGRAM: &str = r#"import { add } from "counter";
import { IoError } from "velt/io";

function main() {
  console.log("count", add(2));
  console.log("count", add(3));
  try {
    add(-1);
  } catch (e) {
    if (e instanceof IoError) {
      console.log("error", e.code);
    }
  }
}
"#;

const EXPECTED: &str = "count 2\ncount 5\nerror EINVAL\n";

fn server(version: &str) -> String {
    format!(
        r#"import {{ serve }} from "http-server";
import {{ add }} from "counter";

async function main() {{
  const server = await serve({{ port: 0 }}, async (req: Request): Promise<Response> => {{
    return new Response(`{version} ${{add(1)}}`);
  }});
  console.log(`listening on ${{server.port}}`);
  await sleep(1000000000);
}}
"#
    )
}

#[test]
fn package_with_native_code() {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    if crate::no_window::command(&cargo)
        .arg("--version")
        .output()
        .is_err()
    {
        // Cargo builds the library under test; skipping must be explicit, never silent.
        assert!(
            std::env::var_os("VELT_SKIP_NATIVE_E2E").is_some(),
            "no cargo to build the native library with (set VELT_SKIP_NATIVE_E2E=1 to skip)"
        );
        eprintln!("skipped (VELT_SKIP_NATIVE_E2E): no cargo");
        return;
    }
    // `velt run` links debug builds against the shared runtime: built and checked here.
    reload_support::build_runtime();
    let tmp = test_dir::TestDir::new();
    let host = velt_codegen_cl::host_triple();

    // The author's machine: build the library from the repository's package (incremental).
    let source = repo().join("crates/veltc/tests/native_package");
    let built = crate::no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["native", "build"])
        .current_dir(&source)
        .env("VELT_CARGO", &cargo)
        .output()
        .expect("velt native build");
    ok(built, "velt native build");
    let artifacts = source.join("target/velt-native");
    assert!(artifacts.join(&host).join("native.json").is_file());

    // A registry server.
    let served = crate::no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["registry", "serve", "--port", "0", "--dir"])
        .arg(tmp.path().join("served"))
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start server");
    let mut served = Kill(served);
    let mut banner = String::new();
    BufReader::new(served.0.stderr.take().unwrap())
        .read_line(&mut banner)
        .expect("banner");
    let url = banner
        .split_whitespace()
        .find(|w| w.starts_with("http://"))
        .expect("url")
        .to_string();

    // Publish a copy that names the server and lists only this machine's target.
    let lib = tmp.path().join("counter");
    copy_dir(&source, &lib);
    let mut manifest = vpm::Manifest::from_dir(&lib).unwrap();
    manifest.registry = Some(url.clone());
    manifest.native.as_mut().unwrap().targets = vec![host.to_string()];
    std::fs::write(lib.join(vpm::manifest::MANIFEST_FILE), manifest.to_velt()).unwrap();
    let home_a = tmp.path().join("author");
    let artifacts_arg = artifacts.to_string_lossy().into_owned();
    let published = velt(
        &lib,
        &home_a,
        &["publish", "--native-artifacts", &artifacts_arg],
    );
    let published = ok(published, "velt publish");
    let text = String::from_utf8_lossy(&published.stderr);
    assert!(
        text.contains(&format!("prebuilt library for {host}")),
        "{text}"
    );

    // The user's machine: no cargo.
    let home = tmp.path().join("user");
    ok(velt(tmp.path(), &home, &["new", "app"]), "velt new");
    let app = tmp.path().join("app");
    let mut manifest = vpm::Manifest::from_dir(&app).unwrap();
    manifest.registry = Some(url.clone());
    std::fs::write(app.join(vpm::manifest::MANIFEST_FILE), manifest.to_velt()).unwrap();
    // Not allowed yet: `velt add` names the line, and adds once it's there.
    let refused = velt(&app, &home, &["add", "counter"]);
    let text = String::from_utf8_lossy(&refused.stderr);
    assert!(!refused.status.success(), "{text}");
    assert!(
        text.contains("package `counter` has native code")
            && text.contains("allowNative: [\"counter\"]"),
        "{text}"
    );
    let mut manifest = vpm::Manifest::from_dir(&app).unwrap();
    manifest.allow_native = vec!["counter".into()];
    std::fs::write(app.join(vpm::manifest::MANIFEST_FILE), manifest.to_velt()).unwrap();
    let added = ok(velt(&app, &home, &["add", "counter"]), "velt add");
    let text = String::from_utf8_lossy(&added.stderr);
    assert!(
        text.contains("`counter` 0.2.0 runs native code (prebuilt, checksum verified"),
        "{text}"
    );
    let lock = std::fs::read_to_string(app.join("velt.lock.json")).unwrap();
    assert!(lock.contains(&format!("\"{host}\": \"sha256:")), "{lock}");

    // Debug build: linked against the shared library.
    std::fs::write(app.join("src/main.velt"), PROGRAM).unwrap();
    let run = ok(velt(&app, &home, &["run"]), "velt run");
    assert_eq!(String::from_utf8_lossy(&run.stdout), EXPECTED);

    // Release build: self-contained, it runs without the cached library. Its own output path:
    // on Windows the debug executable that just ran can stay locked for a moment.
    ok(
        velt(
            &app,
            &home,
            &["build", "--release", "-o", "target/velt-release/app"],
        ),
        "velt build --release",
    );
    let exe = app
        .join("target/velt-release/app")
        .with_extension(std::env::consts::EXE_EXTENSION);
    if !cfg!(windows) {
        std::fs::rename(home.join("cache"), home.join("cache.moved")).unwrap();
        let out = ok(
            crate::no_window::command(&exe).output().unwrap(),
            "the release executable",
        );
        assert_eq!(String::from_utf8_lossy(&out.stdout), EXPECTED);
        std::fs::rename(home.join("cache.moved"), home.join("cache")).unwrap();
    }

    // The server program serves with the repository's HTTP server package, published there too.
    let http = tmp.path().join("http-server");
    copy_dir(&repo().join("packages/http-server"), &http);
    let mut manifest = vpm::Manifest::from_dir(&http).unwrap();
    manifest.registry = Some(url.clone());
    std::fs::write(http.join(vpm::manifest::MANIFEST_FILE), manifest.to_velt()).unwrap();
    ok(
        velt(&http, &home_a, &["publish"]),
        "velt publish http-server",
    );
    ok(
        velt(&app, &home, &["add", "http-server"]),
        "velt add http-server",
    );

    // `velt dev`: the JIT host loads the library; a Velt edit hot-swaps and what the library
    // held before the swap is still there.
    std::fs::write(app.join("main.velt"), server("v1")).unwrap();
    let dev = DevIn::start(&app, &home);
    dev.0
        .wait_stderr(Mark::default(), "velt dev: started")
        .unwrap();
    let port = dev.0.port().unwrap();
    assert_eq!(get(port, "/").unwrap(), "v1 1");
    assert_eq!(get(port, "/").unwrap(), "v1 2");
    let mark = dev.0.mark();
    std::fs::write(app.join("main.velt"), server("v2")).unwrap();
    // The swap that took the edit in: it compiled changed functions. A reload that started
    // before the edit was saved (e.g. one more after the first build) reports
    // `hot-swapped 0 functions` and leaves `v1` running.
    dev.0
        .wait_stderr_where(mark, "a hot swap of changed functions", |line| {
            line.strip_prefix("velt dev: hot-swapped ")
                .and_then(|rest| rest.split(' ').next()?.parse::<usize>().ok())
                .is_some_and(|functions| functions > 0)
        })
        .unwrap();
    assert_eq!(get(port, "/").unwrap(), "v2 3");
    drop(served);
}

/// `velt dev main.velt` in the app, with the user's home and no cargo.
struct DevIn(Dev);

impl DevIn {
    fn start(dir: &Path, home: &Path) -> DevIn {
        std::env::set_var("VELT_HOME", home);
        std::env::set_var("VELT_CARGO", NO_CARGO);
        DevIn(Dev::start(dir, &[]))
    }
}
