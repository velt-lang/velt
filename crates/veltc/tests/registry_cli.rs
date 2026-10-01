//! `velt registry serve` + `velt publish` / `velt add` against it: two "machines" (separate
//! `VELT_HOME`s) share a package over HTTP.

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};

fn velt(dir: &Path, home: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_velt"))
        .args(args)
        .current_dir(dir)
        .env("VELT_HOME", home)
        .env_remove("VELT_REGISTRY")
        .env_remove("VELT_REGISTRY_TOKEN")
        .output()
        .expect("run velt")
}

/// Kills the server when the test ends, also on failure (it would keep the harness's output
/// pipe open).
struct Kill(Child);

impl Drop for Kill {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn share_a_package_through_the_registry_server() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let server = Command::new(env!("CARGO_BIN_EXE_velt"))
        .args(["registry", "serve", "--port", "0", "--dir"])
        .arg(tmp.path().join("served"))
        .env("VELT_REGISTRY_TOKEN", "t0ken")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start server");
    let mut server = Kill(server);
    let mut stderr = BufReader::new(server.0.stderr.take().expect("stderr"));
    let mut line = String::new();
    stderr.read_line(&mut line).expect("banner");
    let url = line
        .split_whitespace()
        .find(|w| w.starts_with("http://"))
        .expect("url in banner")
        .to_string();

    let (home_a, home_b) = (tmp.path().join("a"), tmp.path().join("b"));
    let lib = tmp.path().join("greet");
    assert!(velt(tmp.path(), &home_a, &["new", "greet", "--lib"])
        .status
        .success());
    let manifest = std::fs::read_to_string(lib.join("velt.toml")).expect("manifest");
    std::fs::write(
        lib.join("velt.toml"),
        format!("registry = \"{url}\"\n{manifest}"),
    )
    .expect("write");
    let denied = velt(&lib, &home_a, &["publish"]);
    assert!(!denied.status.success());
    assert!(String::from_utf8_lossy(&denied.stderr).contains("VELT_REGISTRY_TOKEN"));
    let published = Command::new(env!("CARGO_BIN_EXE_velt"))
        .arg("publish")
        .current_dir(&lib)
        .env("VELT_HOME", &home_a)
        .env("VELT_REGISTRY_TOKEN", "t0ken")
        .output()
        .expect("publish");
    assert!(
        published.status.success(),
        "{}",
        String::from_utf8_lossy(&published.stderr)
    );

    let app = tmp.path().join("app");
    assert!(velt(tmp.path(), &home_b, &["new", "app"]).status.success());
    let manifest = std::fs::read_to_string(app.join("velt.toml")).expect("manifest");
    std::fs::write(
        app.join("velt.toml"),
        format!("registry = \"{url}\"\n{manifest}"),
    )
    .expect("write");
    let added = velt(&app, &home_b, &["add", "greet"]);
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );
    let lock = std::fs::read_to_string(app.join("velt.lock")).expect("lock");
    assert!(
        lock.contains("name = \"greet\"") && lock.contains("sha256:"),
        "{lock}"
    );
}
