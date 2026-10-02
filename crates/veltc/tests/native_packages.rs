//! A package with native code end to end (docs/internals/design/native-packages.md), with the
//! prototype driver `packages/sqlite`:
//!
//! 1. the author builds the library (`velt native build`, cargo) and publishes the package with
//!    it to a `velt registry serve`;
//! 2. a user **without cargo** adds the package: the prebuilt library is downloaded, verified
//!    and listed as native code;
//! 3. `velt run` (debug: the shared library), `velt build --release` (static: the executable
//!    still runs after the cache is deleted) and `velt dev` (loaded into the JIT host; a Velt
//!    edit still hot-swaps, and the database opened before the swap is still there).
//!
//! The first run builds SQLite from source for the library (about a minute); later runs reuse
//! cargo's output under `packages/sqlite/target/`. Without cargo it fails, unless
//! `VELT_SKIP_NATIVE_E2E=1`.

#[allow(dead_code)]
mod reload_support;

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};

use reload_support::{get, Dev, Mark};

const NO_CARGO: &str = "velt-test-no-such-cargo";

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn velt(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_velt"))
        .args(args)
        .current_dir(dir)
        .env("VELT_HOME", home)
        .env("VELT_CARGO", NO_CARGO)
        .env_remove("VELT_REGISTRY")
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

const PROGRAM: &str = r#"import { Database, SqliteError, version } from "sqlite";

struct User {
  id: i64;
  name: string;
}

async function main() {
  console.log("sqlite", version().length > 0);
  using db = new Database(":memory:");
  db.exec("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT UNIQUE)");
  db.query<User>("INSERT INTO users (name) VALUES (?), (?)", "[\"ada\", \"grace\"]");
  for (const u of db.query<User>("SELECT id, name FROM users ORDER BY id")) {
    console.log(u.id, u.name);
  }
  const found = await db.ref.query<User>("SELECT id, name FROM users WHERE id = ?", "[2]");
  console.log("async", found[0].name);
  try {
    db.query<User>("INSERT INTO users (name) VALUES (?)", "[\"ada\"]");
  } catch (e) {
    if (e instanceof SqliteError) {
      console.log("error", e.code);
    }
  }
}
"#;

const EXPECTED: &str = "sqlite true\n1 ada\n2 grace\nasync grace\nerror SQLITE_CONSTRAINT_UNIQUE\n";

fn server(version: &str) -> String {
    format!(
        r#"import {{ serve, Request, Response }} from "velt:http";
import {{ Database }} from "sqlite";

struct Count {{
  n: i64;
}}

async function main() {{
  const db = new Database(":memory:");
  db.exec("CREATE TABLE hits (at INTEGER)");
  const r = db.ref;
  const server = await serve({{ port: 0 }}, async (req: Request): Promise<Response> => {{
    await r.query<Count>("INSERT INTO hits (at) VALUES (1)");
    const rows = await r.query<Count>("SELECT count(*) AS n FROM hits");
    return Response.text(`{version} ${{rows[0].n}}`);
  }});
  console.log(`listening on ${{server.port}}`);
  await sleep(1000000000);
}}
"#
    )
}

#[test]
fn sqlite_package_with_native_code() {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    if Command::new(&cargo).arg("--version").output().is_err() {
        // Cargo builds the library under test; skipping must be explicit, never silent.
        assert!(
            std::env::var_os("VELT_SKIP_NATIVE_E2E").is_some(),
            "no cargo to build the native library with (set VELT_SKIP_NATIVE_E2E=1 to skip)"
        );
        eprintln!("skipped (VELT_SKIP_NATIVE_E2E): no cargo");
        return;
    }
    reload_support::build_runtime();
    // `velt run` links debug builds against the shared runtime when it exists: keep it current.
    // Not under the gate (`VELT_RT_PREBUILT=1`), which built it with the workspace: rebuilding it
    // there would replace the import library while tests running in parallel link against it.
    let prebuilt =
        cfg!(debug_assertions) && std::env::var_os("VELT_RT_PREBUILT").is_some_and(|v| v == "1");
    if !cfg!(target_env = "musl") && !prebuilt {
        let profile: &[&str] = if cfg!(debug_assertions) {
            &[]
        } else {
            &["--release"]
        };
        let status = Command::new(&cargo)
            .args(["build", "-q", "-p", "velt_rt_shared"])
            .args(profile)
            .status()
            .expect("run cargo");
        assert!(status.success(), "cargo build -p velt_rt_shared failed");
    }
    let tmp = tempfile::tempdir().expect("temp dir");
    let host = velt_codegen_cl::host_triple();

    // The author's machine: build the library from the repository's package (incremental).
    let source = repo().join("packages/sqlite");
    let built = Command::new(env!("CARGO_BIN_EXE_velt"))
        .args(["native", "build"])
        .current_dir(&source)
        .env("VELT_CARGO", &cargo)
        .output()
        .expect("velt native build");
    ok(built, "velt native build");
    let artifacts = source.join("target/velt-native");
    assert!(artifacts.join(&host).join("native.toml").is_file());

    // A registry server.
    let served = Command::new(env!("CARGO_BIN_EXE_velt"))
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
    let lib = tmp.path().join("sqlite");
    copy_dir(&source, &lib);
    let mut manifest = vpm::Manifest::from_dir(&lib).unwrap();
    manifest.registry = Some(url.clone());
    manifest.native.as_mut().unwrap().targets = vec![host.to_string()];
    std::fs::write(lib.join(vpm::manifest::MANIFEST_FILE), manifest.to_vlt()).unwrap();
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
    std::fs::write(app.join(vpm::manifest::MANIFEST_FILE), manifest.to_vlt()).unwrap();
    let added = ok(velt(&app, &home, &["add", "sqlite"]), "velt add");
    let text = String::from_utf8_lossy(&added.stderr);
    assert!(
        text.contains("`sqlite` 0.1.0 runs native code (prebuilt, checksum verified"),
        "{text}"
    );
    let lock = std::fs::read_to_string(app.join("velt.lock")).unwrap();
    assert!(lock.contains(&format!("{host} = \"sha256:")), "{lock}");

    // Debug build: linked against the shared library.
    std::fs::write(app.join("src/main.vlt"), PROGRAM).unwrap();
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
            Command::new(&exe).output().unwrap(),
            "the release executable",
        );
        assert_eq!(String::from_utf8_lossy(&out.stdout), EXPECTED);
        std::fs::rename(home.join("cache.moved"), home.join("cache")).unwrap();
    }

    // `velt dev`: the JIT host loads the library; a Velt edit hot-swaps and the database the
    // program opened before the swap is still there.
    std::fs::write(app.join("main.vlt"), server("v1")).unwrap();
    let dev = DevIn::start(&app, &home);
    dev.0
        .wait_stderr(Mark::default(), "velt dev: started")
        .unwrap();
    let port = dev.0.port().unwrap();
    assert_eq!(get(port, "/").unwrap(), "v1 1");
    assert_eq!(get(port, "/").unwrap(), "v1 2");
    let mark = dev.0.mark();
    std::fs::write(app.join("main.vlt"), server("v2")).unwrap();
    dev.0.wait_stderr(mark, "velt dev: hot-swapped").unwrap();
    assert_eq!(get(port, "/").unwrap(), "v2 3");
    drop(served);
}

/// `velt dev main.vlt` in the app, with the user's home and no cargo.
struct DevIn(Dev);

impl DevIn {
    fn start(dir: &Path, home: &Path) -> DevIn {
        std::env::set_var("VELT_HOME", home);
        std::env::set_var("VELT_CARGO", NO_CARGO);
        DevIn(Dev::start(dir, &[]))
    }
}
