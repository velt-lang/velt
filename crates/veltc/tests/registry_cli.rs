//! `velt registry serve` + `velt publish` / `velt add` against it: two "machines" (separate
//! `VELT_HOME`s) share a package over HTTP; a registry user publishes, owns, yanks and searches.

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};

mod test_dir;

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

/// `velt` with the registry token `token`.
fn velt_as(token: &str, dir: &Path, home: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_velt"))
        .args(args)
        .current_dir(dir)
        .env("VELT_HOME", home)
        .env_remove("VELT_REGISTRY")
        .env("VELT_REGISTRY_TOKEN", token)
        .output()
        .expect("run velt")
}

/// `velt login <url>` (or `logout`) with `token` on stdin.
fn velt_login(home: &Path, sub: &str, url: &str, token: &str) -> std::process::Output {
    use std::io::Write;
    let mut child = Command::new(env!("CARGO_BIN_EXE_velt"))
        .args([sub, url])
        .env("VELT_HOME", home)
        .env_remove("VELT_REGISTRY_TOKEN")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run velt");
    let mut stdin = child.stdin.take().expect("stdin");
    stdin
        .write_all(
            format!(
                "{token}
"
            )
            .as_bytes(),
        )
        .expect("token");
    drop(stdin);
    child.wait_with_output().expect("run velt")
}

fn text(out: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn share_a_package_through_the_registry_server() {
    let tmp = test_dir::TestDir::new();
    let served = tmp.path().join("served");
    let dir_arg = served.to_string_lossy().into_owned();
    let added_user = velt(
        tmp.path(),
        &tmp.path().join("admin"),
        &["registry", "user", "add", "alice", "--dir", &dir_arg],
    );
    assert!(added_user.status.success(), "{}", text(&added_user));
    let token = String::from_utf8_lossy(&added_user.stdout)
        .trim()
        .to_string();
    assert_eq!(token.len(), 64, "{token}");
    let server = Command::new(env!("CARGO_BIN_EXE_velt"))
        .args(["registry", "serve", "--port", "0", "--dir"])
        .arg(&served)
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
    let mut manifest = vpm::Manifest::from_dir(&lib).expect("manifest");
    manifest.registry = Some(url.clone());
    std::fs::write(lib.join(vpm::manifest::MANIFEST_FILE), manifest.to_vlt()).expect("write");
    let denied = velt(&lib, &home_a, &["publish"]);
    assert!(!denied.status.success());
    assert!(String::from_utf8_lossy(&denied.stderr).contains("VELT_REGISTRY_TOKEN"));
    assert!(text(&denied).contains("velt login"), "{}", text(&denied));
    let login = velt_login(&home_a, "login", &url, &token);
    assert!(login.status.success(), "{}", text(&login));
    assert!(home_a.join("credentials.json").is_file());
    // The stored token goes only to the registry it was stored for: `localhost` is the same
    // server under another name, so it gets none.
    let port = url.rsplit(':').next().expect("port");
    let mut manifest = vpm::Manifest::from_dir(&lib).expect("manifest");
    manifest.registry = Some(format!("http://localhost:{port}"));
    std::fs::write(lib.join(vpm::manifest::MANIFEST_FILE), manifest.to_vlt()).expect("write");
    let elsewhere = velt(&lib, &home_a, &["publish"]);
    assert!(
        text(&elsewhere).contains("refused the request"),
        "{}",
        text(&elsewhere)
    );
    manifest.registry = Some(url.clone());
    std::fs::write(lib.join(vpm::manifest::MANIFEST_FILE), manifest.to_vlt()).expect("write");
    let published = velt(&lib, &home_a, &["publish"]);
    assert!(published.status.success(), "{}", text(&published));
    let owners = velt(&lib, &home_a, &["owner", "list", "greet"]);
    assert_eq!(String::from_utf8_lossy(&owners.stdout), "alice\n");
    let found = velt(&lib, &home_a, &["search", "gre"]);
    assert!(
        String::from_utf8_lossy(&found.stdout).contains("greet  0.1.0"),
        "{}",
        text(&found)
    );

    let app = tmp.path().join("app");
    assert!(velt(tmp.path(), &home_b, &["new", "app"]).status.success());
    let mut manifest = vpm::Manifest::from_dir(&app).expect("manifest");
    manifest.registry = Some(url.clone());
    std::fs::write(app.join(vpm::manifest::MANIFEST_FILE), manifest.to_vlt()).expect("write");
    let added = velt(&app, &home_b, &["add", "greet"]);
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );
    let lock = std::fs::read_to_string(app.join("velt.lock.json")).expect("lock");
    assert!(
        lock.contains("\"name\": \"greet\"") && lock.contains("sha256:"),
        "{lock}"
    );

    // A yanked version: the locked app keeps installing it, a new requirement can't pick it.
    // Machine B has no token for the registry.
    let denied = velt(&lib, &home_b, &["yank", "greet@0.1.0"]);
    assert!(
        text(&denied).contains("VELT_REGISTRY_TOKEN"),
        "{}",
        text(&denied)
    );
    let yanked = velt(&lib, &home_a, &["yank", "greet@0.1.0"]);
    assert!(yanked.status.success(), "{}", text(&yanked));
    let installed = velt(&app, &home_b, &["install", "--locked"]);
    assert!(installed.status.success(), "{}", text(&installed));
    assert!(
        text(&installed).contains("warning: `greet` 0.1.0 is yanked (pinned by velt.lock.json)"),
        "{}",
        text(&installed)
    );
    let other = tmp.path().join("other");
    assert!(velt(tmp.path(), &home_b, &["new", "other"])
        .status
        .success());
    let mut manifest = vpm::Manifest::from_dir(&other).expect("manifest");
    manifest.registry = Some(url.clone());
    std::fs::write(other.join(vpm::manifest::MANIFEST_FILE), manifest.to_vlt()).expect("write");
    let refused = velt(&other, &home_b, &["add", "greet@0.1"]);
    assert!(
        text(&refused).contains("0.1.0 (yanked)"),
        "{}",
        text(&refused)
    );
    let unyanked = velt(&lib, &home_a, &["yank", "greet@0.1.0", "--undo"]);
    assert!(unyanked.status.success(), "{}", text(&unyanked));
    // After `velt logout`, writes are refused again; $VELT_REGISTRY_TOKEN still works (CI).
    let logout = velt_login(&home_a, "logout", &url, "");
    assert!(logout.status.success(), "{}", text(&logout));
    assert!(!velt(&lib, &home_a, &["yank", "greet@0.1.0"])
        .status
        .success());
    let yanked = velt_as(&token, &lib, &home_a, &["yank", "greet@0.1.0"]);
    assert!(yanked.status.success(), "{}", text(&yanked));
    let unyanked = velt_as(&token, &lib, &home_a, &["yank", "greet@0.1.0", "--undo"]);
    assert!(unyanked.status.success(), "{}", text(&unyanked));
    assert!(velt(&other, &home_b, &["add", "greet@0.1"])
        .status
        .success());
}

#[test]
fn tokens_never_travel_over_plain_http_to_another_machine() {
    let tmp = test_dir::TestDir::new();
    let home = tmp.path().join("home");
    let refused = velt_login(&home, "login", "http://registry.example.com", "secret");
    assert!(!refused.status.success());
    assert!(
        text(&refused).contains("refusing to send a registry token"),
        "{}",
        text(&refused)
    );
    assert!(!home.join("credentials.json").exists());
    // $VELT_REGISTRY_TOKEN is not sent there either: the write fails before connecting (to an
    // unroutable documentation address).
    let lib = tmp.path().join("lib");
    assert!(velt(tmp.path(), &home, &["new", "lib", "--lib"])
        .status
        .success());
    let mut manifest = vpm::Manifest::from_dir(&lib).expect("manifest");
    manifest.registry = Some("http://192.0.2.1:9".into());
    std::fs::write(lib.join(vpm::manifest::MANIFEST_FILE), manifest.to_vlt()).expect("write");
    let out = velt_as("secret", &lib, &home, &["yank", "lib@0.1.0"]);
    assert!(
        text(&out).contains("refusing to send $VELT_REGISTRY_TOKEN to http://192.0.2.1:9"),
        "{}",
        text(&out)
    );
}

#[test]
fn a_token_without_users_does_not_start_an_open_server() {
    let tmp = test_dir::TestDir::new();
    let served = tmp.path().join("served");
    let refused = velt_as(
        "old-shared-token",
        tmp.path(),
        &tmp.path().join("home"),
        &[
            "registry",
            "serve",
            "--port",
            "0",
            "--dir",
            &served.to_string_lossy(),
        ],
    );
    assert!(!refused.status.success());
    assert!(
        text(&refused).contains("velt registry user add"),
        "{}",
        text(&refused)
    );
}
