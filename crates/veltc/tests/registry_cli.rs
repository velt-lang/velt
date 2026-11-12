//! `velt registry serve` + `velt publish` / `velt add` against it: two "machines" (separate
//! `VELT_HOME`s) share a package over HTTP; a registry user publishes, owns, yanks and searches.

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Stdio};

mod no_window;
mod test_dir;

fn velt(dir: &Path, home: &Path, args: &[&str]) -> std::process::Output {
    crate::no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(args)
        .current_dir(dir)
        .env("VELT_HOME", home)
        .env("VELT_REGISTRY", home.join("registry"))
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
    crate::no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(args)
        .current_dir(dir)
        .env("VELT_HOME", home)
        .env("VELT_REGISTRY", home.join("registry"))
        .env("VELT_REGISTRY_TOKEN", token)
        .output()
        .expect("run velt")
}

/// `velt <sub> <url>` (`login` or `logout`) with stdin, stdout and stderr piped.
fn spawn_login(home: &Path, sub: &str, url: &str) -> Child {
    crate::no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args([sub, url])
        .env("VELT_HOME", home)
        .env_remove("VELT_REGISTRY_TOKEN")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run velt")
}

/// `velt login <url>` (or `logout`) with `token` on stdin.
fn velt_login(home: &Path, sub: &str, url: &str, token: &str) -> std::process::Output {
    use std::io::Write;
    let mut child = spawn_login(home, sub, url);
    let mut stdin = child.stdin.take().expect("stdin");
    let line = format!("{token}\n");
    match stdin.write_all(line.as_bytes()) {
        // A `velt` that exits without reading stdin closes the pipe; its status and output
        // tell the callers what happened.
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {}
        written => written.expect("token"),
    }
    drop(stdin);
    child.wait_with_output().expect("run velt")
}

/// `velt login <url>` that must refuse without reading the token: it exits while stdin is
/// still open and nothing has been written to it. The deadline only guards against a hang (a
/// `velt` waiting for the token).
fn velt_login_refused(home: &Path, url: &str) -> std::process::Output {
    let mut child = spawn_login(home, "login", url);
    let stdin = child.stdin.take().expect("stdin");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    while child.try_wait().expect("velt login").is_none() {
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            panic!("velt login {url} waits for a token instead of refusing");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
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
    let server = crate::no_window::command(env!("CARGO_BIN_EXE_velt"))
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
    std::fs::write(lib.join(vpm::manifest::MANIFEST_FILE), manifest.to_velt()).expect("write");
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
    std::fs::write(lib.join(vpm::manifest::MANIFEST_FILE), manifest.to_velt()).expect("write");
    let elsewhere = velt(&lib, &home_a, &["publish"]);
    assert!(
        text(&elsewhere).contains("refused the request"),
        "{}",
        text(&elsewhere)
    );
    manifest.registry = Some(url.clone());
    std::fs::write(lib.join(vpm::manifest::MANIFEST_FILE), manifest.to_velt()).expect("write");
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
    std::fs::write(app.join(vpm::manifest::MANIFEST_FILE), manifest.to_velt()).expect("write");
    let added = velt(&app, &home_b, &["add", "greet"]);
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );
    // Errors name the registry asked, by its URL.
    let missing = velt(&app, &home_b, &["add", "nothing-here"]);
    assert!(
        text(&missing).contains(&format!(
            "package `nothing-here` is not in the registry `{url}`"
        )),
        "{}",
        text(&missing)
    );
    let mut manifest = vpm::Manifest::from_dir(&app).expect("manifest");
    let mut with_missing = manifest.clone();
    with_missing
        .dependencies
        .insert("nothing-here".into(), vpm::Dependency::Version("1".into()));
    std::fs::write(
        app.join(vpm::manifest::MANIFEST_FILE),
        with_missing.to_velt(),
    )
    .expect("write");
    let unresolved = velt(&app, &home_b, &["install"]);
    assert!(
        text(&unresolved).contains(&format!(
            "package `nothing-here` is not in the registry `{url}` (required by"
        )),
        "{}",
        text(&unresolved)
    );
    manifest.registry = Some(url.clone());
    std::fs::write(app.join(vpm::manifest::MANIFEST_FILE), manifest.to_velt()).expect("write");
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
    std::fs::write(other.join(vpm::manifest::MANIFEST_FILE), manifest.to_velt()).expect("write");
    let refused = velt(&other, &home_b, &["add", "greet@0.1"]);
    assert!(
        text(&refused).contains("0.1.0 (yanked)"),
        "{}",
        text(&refused)
    );
    let unyanked = velt(&lib, &home_a, &["yank", "greet@0.1.0", "--undo"]);
    assert!(unyanked.status.success(), "{}", text(&unyanked));
    // `velt logout` revokes the token on the registry and forgets it: writes are refused again,
    // also with that token copied elsewhere. A new token works, from $VELT_REGISTRY_TOKEN too (CI).
    let logout = velt_login(&home_a, "logout", &url, "");
    assert!(logout.status.success(), "{}", text(&logout));
    assert!(text(&logout).contains("Revoked"), "{}", text(&logout));
    assert!(!velt(&lib, &home_a, &["yank", "greet@0.1.0"])
        .status
        .success());
    let copied = velt_as(&token, &lib, &home_a, &["yank", "greet@0.1.0"]);
    assert!(
        text(&copied).contains("refused the request"),
        "{}",
        text(&copied)
    );
    let again = velt_login(&home_a, "logout", &url, "");
    assert!(text(&again).contains("no token stored"), "{}", text(&again));
    let rotated = velt(
        tmp.path(),
        &tmp.path().join("admin"),
        &["registry", "user", "token", "alice", "--dir", &dir_arg],
    );
    assert!(rotated.status.success(), "{}", text(&rotated));
    let token = String::from_utf8_lossy(&rotated.stdout).trim().to_string();
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
    // `velt login` refuses before it reads the token: it never asks for one it would not send.
    let refused = velt_login_refused(&home, "http://registry.example.com");
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
    std::fs::write(lib.join(vpm::manifest::MANIFEST_FILE), manifest.to_velt()).expect("write");
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

/// `velt login` / `velt logout` without a URL in `dir`, with `token` on stdin and
/// `$VELT_REGISTRY` set to `registry` (or unset).
fn velt_login_here(
    dir: &Path,
    home: &Path,
    sub: &str,
    registry: Option<&str>,
    token: &str,
) -> std::process::Output {
    use std::io::Write;
    let mut cmd = crate::no_window::command(env!("CARGO_BIN_EXE_velt"));
    cmd.arg(sub)
        .current_dir(dir)
        .env("VELT_HOME", home)
        .env_remove("VELT_REGISTRY_TOKEN")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    match registry {
        Some(r) => cmd.env("VELT_REGISTRY", r),
        None => cmd.env_remove("VELT_REGISTRY"),
    };
    let mut child = cmd.spawn().expect("run velt");
    let mut stdin = child.stdin.take().expect("stdin");
    match stdin.write_all(format!("{token}\n").as_bytes()) {
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {}
        written => written.expect("token"),
    }
    drop(stdin);
    child.wait_with_output().expect("run velt")
}

/// Without a URL, `velt login` stores the token for the registry `velt publish` would use:
/// https://registry.veltpm.com by default (storing a token asks no registry), the package's
/// `registry`, and never a local directory registry.
#[test]
fn login_without_a_url_signs_in_to_the_packages_registry_by_default_veltpm() {
    let tmp = test_dir::TestDir::new();
    let home = tmp.path().join("home");
    let stored =
        |home: &Path| std::fs::read_to_string(home.join("credentials.json")).unwrap_or_default();
    let login = velt_login_here(tmp.path(), &home, "login", None, "t0ken");
    assert!(login.status.success(), "{}", text(&login));
    assert!(
        text(&login).contains("the token for https://registry.veltpm.com"),
        "{}",
        text(&login)
    );
    assert!(
        stored(&home).contains("\"https://registry.veltpm.com\""),
        "{}",
        stored(&home)
    );

    // In a package whose `registry` is a server on this machine (nothing listens on port 9:
    // `logout` can't revoke the token there, warns, and removes it).
    let lib = tmp.path().join("lib");
    assert!(velt(tmp.path(), &home, &["new", "lib", "--lib"])
        .status
        .success());
    let mut manifest = vpm::Manifest::from_dir(&lib).expect("manifest");
    manifest.registry = Some("http://127.0.0.1:9".into());
    std::fs::write(lib.join(vpm::manifest::MANIFEST_FILE), manifest.to_velt()).expect("write");
    let login = velt_login_here(&lib, &home, "login", None, "t0ken");
    assert!(login.status.success(), "{}", text(&login));
    assert!(
        stored(&home).contains("\"http://127.0.0.1:9\""),
        "{}",
        stored(&home)
    );
    let logout = velt_login_here(&lib, &home, "logout", None, "");
    assert!(text(&logout).contains("Removed"), "{}", text(&logout));
    assert!(!stored(&home).contains("127.0.0.1:9"), "{}", stored(&home));

    // `$VELT_REGISTRY` set to a directory, outside a package: no server to sign in to.
    let dir = tmp.path().join("registry").to_string_lossy().into_owned();
    let refused = velt_login_here(tmp.path(), &home, "login", Some(&dir), "t0ken");
    assert!(!refused.status.success());
    assert!(
        text(&refused).contains("is a local directory registry"),
        "{}",
        text(&refused)
    );
}
