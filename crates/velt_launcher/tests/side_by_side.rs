//! The launcher against fake releases served from this machine (#948): packages pinned to
//! different versions run different toolchains, missing versions are downloaded and checked,
//! and `velt toolchain` manages them.

use std::collections::HashMap;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};

use velt_http::{Response, Server};

const EXE: &str = std::env::consts::EXE_SUFFIX;

/// The stand-in toolchain binary (examples/fake_velt.rs), built by `cargo test` beside the
/// launcher.
fn fake_velt() -> PathBuf {
    let launcher = Path::new(env!("CARGO_BIN_EXE_velt-launcher"));
    let exe = launcher
        .parent()
        .unwrap()
        .join("examples")
        .join(format!("fake_velt{EXE}"));
    assert!(exe.is_file(), "{} is not built", exe.display());
    exe
}

fn host() -> String {
    velt_toolchain::release::host_triple()
}

/// `velt-<v>-<host>.tar.gz` holding the fake toolchain.
fn archive(version: &str) -> Vec<u8> {
    let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    let mut tar = tar::Builder::new(gz);
    let top = format!("velt-{version}-{}", host());
    let mut add = |path: String, bytes: &[u8], mode: u32| {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(mode);
        tar.append_data(&mut header, path, bytes).unwrap();
    };
    add(
        format!("{top}/bin/velt{EXE}"),
        &std::fs::read(fake_velt()).unwrap(),
        0o755,
    );
    add(format!("{top}/std/VERSION"), version.as_bytes(), 0o644);
    tar.into_inner().unwrap().finish().unwrap()
}

/// Releases served over loopback HTTP, laid out like GitHub's, signed with a key of their own.
struct Releases {
    server: Option<Server>,
    /// The public key, hex (`$VELT_INSTALL_PUBLIC_KEY`).
    key: String,
    signer: ring::signature::Ed25519KeyPair,
    files: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    requests: Arc<Mutex<Vec<String>>>,
}

impl Releases {
    fn new(versions: &[&str]) -> Releases {
        let files: Arc<Mutex<HashMap<String, Vec<u8>>>> = Arc::default();
        let requests: Arc<Mutex<Vec<String>>> = Arc::default();
        use ring::signature::KeyPair;
        let pkcs8 =
            ring::signature::Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new())
                .unwrap();
        let signer = ring::signature::Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let key: String = signer
            .public_key()
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let mut map = files.lock().unwrap();
        for v in versions {
            let name = format!("velt-{v}-{}.tar.gz", host());
            let bytes = archive(v);
            let sums = format!("{}  {name}\n", velt_toolchain::install::sha256_hex(&bytes));
            map.insert(format!("/releases/download/v{v}/{name}"), bytes);
            map.insert(
                format!("/releases/download/v{v}/SHA256SUMS.sig"),
                signer.sign(sums.as_bytes()).as_ref().to_vec(),
            );
            map.insert(
                format!("/releases/download/v{v}/SHA256SUMS"),
                sums.into_bytes(),
            );
        }
        drop(map);
        let (f, r) = (files.clone(), requests.clone());
        let handler = Arc::new(move |req: velt_http::Request| {
            r.lock().unwrap().push(req.path.clone());
            match f.lock().unwrap().get(&req.path) {
                Some(bytes) => Response::bytes(200, "application/octet-stream", bytes.clone()),
                None => Response::text(404, "not found"),
            }
        });
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let server = Server::start(listener, handler, 1 << 20).unwrap();
        let releases = Releases {
            server: Some(server),
            key,
            signer,
            files,
            requests,
        };
        let listed: Vec<(&str, Option<&str>)> = versions.iter().map(|v| (*v, None)).collect();
        releases.index(&listed);
        releases
    }

    /// Publish the signed index of releases: (version, why it was yanked).
    fn index(&self, releases: &[(&str, Option<&str>)]) {
        let entries: Vec<String> = releases
            .iter()
            .map(|(v, yanked)| match yanked {
                Some(why) => format!("{{\"version\": \"{v}\", \"yanked\": \"{why}\"}}"),
                None => format!("{{\"version\": \"{v}\"}}"),
            })
            .collect();
        let index = format!("{{\"format\": 1, \"releases\": [{}]}}", entries.join(", "));
        let path = "/releases/download/index/releases.json";
        let mut files = self.files.lock().unwrap();
        files.insert(
            format!("{path}.sig"),
            self.signer.sign(index.as_bytes()).as_ref().to_vec(),
        );
        files.insert(path.into(), index.into_bytes());
    }

    fn url(&self) -> String {
        format!("http://{}", self.server.as_ref().unwrap().addr())
    }

    fn downloads(&self) -> usize {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|p| p.ends_with(".tar.gz"))
            .count()
    }
}

impl Drop for Releases {
    fn drop(&mut self) {
        if let Some(server) = self.server.take() {
            server.stop();
        }
    }
}

/// A root with the launcher in `bin/`, and a work directory.
struct Machine {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    work: PathBuf,
    base: String,
    key: String,
}

impl Machine {
    fn new(releases: &Releases) -> Machine {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::fs::copy(
            env!("CARGO_BIN_EXE_velt-launcher"),
            root.join(format!("bin/velt{EXE}")),
        )
        .unwrap();
        let work = tmp.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        Machine {
            root,
            work,
            base: releases.url(),
            key: releases.key.clone(),
            _tmp: tmp,
        }
    }

    /// A package `name` in the work directory pinned to `velt` (no field when `None`).
    fn package(&self, name: &str, velt: Option<&str>) -> PathBuf {
        let dir = self.work.join(name);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        let pin = velt.map_or(String::new(), |v| format!(", velt: \"{v}\""));
        std::fs::write(
            dir.join("package.vlt"),
            format!(
                "import type {{ Package }} from \"velt:package\";\n\nexport const pkg: Package = \
                 {{ name: \"{name}\", version: \"0.1.0\"{pin} }};\n"
            ),
        )
        .unwrap();
        dir
    }

    fn command(&self, dir: &Path, args: &[&str]) -> Command {
        let mut c = Command::new(self.root.join(format!("bin/velt{EXE}")));
        c.args(args)
            .current_dir(dir)
            .env("VELT_INSTALL_BASE_URL", &self.base)
            .env("VELT_INSTALL_PUBLIC_KEY", &self.key)
            .env_remove("VELT_TOOLCHAIN")
            .env_remove("VELT_TOOLCHAIN_AUTO_INSTALL");
        c
    }

    fn run(&self, dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
        let mut c = self.command(dir, args);
        for (k, v) in env {
            c.env(k, v);
        }
        c.output().unwrap()
    }

    fn ok(&self, dir: &Path, args: &[&str]) -> String {
        self.ok_with(dir, args, &[])
    }

    fn ok_with(&self, dir: &Path, args: &[&str], env: &[(&str, &str)]) -> String {
        let out = self.run(dir, args, env);
        let (stdout, stderr) = text(&out);
        assert!(
            out.status.success(),
            "velt {args:?} failed:\n{stdout}\n{stderr}"
        );
        stdout
    }

    fn fail(&self, dir: &Path, args: &[&str], env: &[(&str, &str)]) -> String {
        let out = self.run(dir, args, env);
        let (stdout, stderr) = text(&out);
        assert!(!out.status.success(), "velt {args:?} succeeded:\n{stdout}");
        stderr
    }

    fn toolchain(&self, version: &str) -> PathBuf {
        self.root.join("toolchains").join(version)
    }
}

fn text(out: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The `prefix=` line the fake toolchain prints.
fn prefix_of(stdout: &str) -> PathBuf {
    let line = stdout
        .lines()
        .find_map(|l| l.strip_prefix("prefix="))
        .unwrap_or_else(|| panic!("not run by a toolchain:\n{stdout}"));
    PathBuf::from(line)
}

fn same(a: &Path, b: &Path) -> bool {
    a.canonicalize().unwrap() == b.canonicalize().unwrap()
}

#[test]
fn packages_pinned_to_different_versions_run_their_own_toolchains() {
    let releases = Releases::new(&["0.1.0", "0.1.2", "0.2.0"]);
    let m = Machine::new(&releases);
    let a = m.package("a", Some("0.1"));
    let b = m.package("b", Some("0.2"));

    // First use downloads the newest 0.1.x, says so on stderr, and passes the arguments on.
    let out = m.run(&a.join("src"), &["build", "--release", "x y"], &[]);
    let (stdout, stderr) = text(&out);
    assert!(out.status.success(), "{stdout}\n{stderr}");
    assert!(stderr.contains("installing velt 0.1.2"), "{stderr}");
    assert!(same(&prefix_of(&stdout), &m.toolchain("0.1.2")), "{stdout}");
    assert!(stdout.contains("args=build --release x y"), "{stdout}");
    assert!(
        stdout.contains("selected=0.1.2 (velt: \"0.1\" in "),
        "{stdout}"
    );
    assert!(m.toolchain("0.1.2").join("std/VERSION").is_file());
    #[cfg(unix)]
    {
        let launcher = m.root.join("bin/velt");
        assert!(
            stdout.contains(&format!("launcher={}", launcher.display())),
            "{stdout}"
        );
    }

    let stdout = m.ok(&b, &["check"]);
    assert!(same(&prefix_of(&stdout), &m.toolchain("0.2.0")), "{stdout}");
    assert_eq!(releases.downloads(), 2);

    // Installed versions are used as they are: no more downloads, and a newer version
    // installed later doesn't move package `a`.
    let stdout = m.ok(&a, &["run"]);
    assert!(same(&prefix_of(&stdout), &m.toolchain("0.1.2")));
    assert_eq!(releases.downloads(), 2);

    // The toolchain's exit code is the command's.
    let out = m.run(&a, &["test", "--exit=3"], &[]);
    assert_eq!(out.status.code(), Some(3));

    // $VELT_TOOLCHAIN overrides the pin for one command.
    let stdout = m.ok_with(&a, &["run"], &[("VELT_TOOLCHAIN", "0.2.0")]);
    assert!(same(&prefix_of(&stdout), &m.toolchain("0.2.0")));
    assert!(
        stdout.contains("selected=0.2.0 ($VELT_TOOLCHAIN)"),
        "{stdout}"
    );
    // An exact pin installs exactly that version.
    let exact = m.package("exact", Some("=0.1.0"));
    let stdout = m.ok(&exact, &["run"]);
    assert!(same(&prefix_of(&stdout), &m.toolchain("0.1.0")));
}

#[test]
fn missing_versions_with_auto_install_off_name_the_install_command() {
    let releases = Releases::new(&["0.1.0"]);
    let m = Machine::new(&releases);
    let a = m.package("a", Some("0.1"));
    let off = [("VELT_TOOLCHAIN_AUTO_INSTALL", "0")];
    let err = m.fail(&a, &["build"], &off);
    assert!(
        err.contains("no installed velt matches `0.1`")
            && err.contains("velt toolchain install 0.1"),
        "{err}"
    );
    let err = m.fail(&a, &["build"], &[("VELT_TOOLCHAIN", "0.1.0"), off[0]]);
    assert!(err.contains("velt toolchain install 0.1.0"), "{err}");
    assert_eq!(releases.downloads(), 0);
    m.ok(&a, &["toolchain", "install", "0.1"]);
    m.ok_with(&a, &["build"], &off);
}

#[test]
fn a_pin_no_release_satisfies_lists_the_published_versions() {
    let releases = Releases::new(&["0.1.0", "0.2.0"]);
    let m = Machine::new(&releases);
    let pkg = m.package("future", Some("0.9"));
    let err = m.fail(&pkg, &["build"], &[]);
    assert!(
        err.contains("no published velt matches `0.9`")
            && err.contains("the published versions are 0.1.0, 0.2.0"),
        "{err}"
    );
    let bad = m.package("bad", Some("latest"));
    let err = m.fail(&bad, &["build"], &[]);
    assert!(
        err.contains("package.vlt:3: `velt`: `latest` is not"),
        "{err}"
    );
}

#[test]
fn a_tampered_archive_is_refused_and_nothing_is_installed() {
    let releases = Releases::new(&["0.1.0"]);
    let name = format!("/releases/download/v0.1.0/velt-0.1.0-{}.tar.gz", host());
    releases
        .files
        .lock()
        .unwrap()
        .insert(name, archive("0.0.666"));
    let m = Machine::new(&releases);
    let err = m.fail(&m.work, &["toolchain", "install", "0.1.0"], &[]);
    assert!(err.contains("SHA-256"), "{err}");
    assert!(!m.toolchain("0.1.0").exists());
    let err = m.fail(&m.work, &["toolchain", "install", "0.3.0"], &[]);
    assert!(err.contains("velt 0.3.0 has no SHA256SUMS"), "{err}");
    // Signed with another key (a mirror's): refused before any archive is downloaded.
    let other = Releases::new(&["0.1.0"]);
    let err = m.fail(
        &m.work,
        &["toolchain", "install", "0.1.0"],
        &[("VELT_INSTALL_BASE_URL", &other.url())],
    );
    assert!(err.contains("does not match the release key"), "{err}");
    assert_eq!(other.downloads(), 0);
    assert!(!m.toolchain("0.1.0").exists());
}

#[test]
fn the_toolchain_commands() {
    let releases = Releases::new(&["0.1.0", "0.1.1", "0.2.0"]);
    let m = Machine::new(&releases);
    let loose = m.work.clone();

    // Outside a package, without a default, nothing is selected.
    let err = m.fail(&loose, &["run", "x.vlt"], &[]);
    assert!(err.contains("no velt toolchain is selected"), "{err}");
    assert!(m
        .ok(&loose, &["toolchain", "list"])
        .contains("no toolchains installed"));

    // The first install becomes the default; loose files use it.
    let out = m.ok(&loose, &["toolchain", "install", "0.1"]);
    assert!(
        out.contains("installed velt 0.1.1") && out.contains("0.1.1 is the default"),
        "{out}"
    );
    let stdout = m.ok(&loose, &["run", "x.vlt"]);
    assert!(same(&prefix_of(&stdout), &m.toolchain("0.1.1")));
    assert!(stdout.contains("selected=0.1.1 (the default)"), "{stdout}");
    let out = m.ok(&loose, &["toolchain", "install", "0.2.0"]);
    assert!(!out.contains("is the default"), "{out}");
    assert!(m
        .ok(&loose, &["toolchain", "install", "0.2.0"])
        .contains("already installed"));

    let pkg = m.package("app", Some("0.2"));
    let list = m.ok(&pkg, &["toolchain", "list"]);
    assert_eq!(list, " > 0.2.0\n*  0.1.1\n", "{list}");
    let which = m.ok(&pkg, &["toolchain", "which"]);
    assert!(which.starts_with("0.2.0 (velt: \"0.2\" in "), "{which}");
    assert!(which.contains("toolchains"), "{which}");
    let which = m.ok(&loose, &["toolchain", "which"]);
    assert!(which.starts_with("0.1.1 (the default)"), "{which}");
    let which = m.ok(&m.package("old", Some("=0.1.0")), &["toolchain", "which"]);
    assert!(
        which.contains("not installed; the next velt command installs it"),
        "{which}"
    );

    let available = m.ok(&loose, &["toolchain", "list", "--available"]);
    assert_eq!(available, "0.2.0  (installed)\n0.1.1  (installed)\n0.1.0\n");

    // default / remove.
    assert_eq!(m.ok(&loose, &["toolchain", "default"]), "0.1.1\n");
    let err = m.fail(&loose, &["toolchain", "remove", "0.1.1"], &[]);
    assert!(err.contains("is the default"), "{err}");
    let err = m.fail(&loose, &["toolchain", "default", "0.1.0"], &[]);
    assert!(err.contains("not installed"), "{err}");
    m.ok(&loose, &["toolchain", "default", "0.2.0"]);
    let out = m.ok(&loose, &["toolchain", "remove", "0.1.1"]);
    assert!(
        out.contains("debug executables it built no longer run"),
        "{out}"
    );
    assert!(!m.toolchain("0.1.1").exists());

    // A prefix built elsewhere, linked under a name.
    let checkout = m.work.join("checkout");
    std::fs::create_dir_all(checkout.join("bin")).unwrap();
    std::fs::copy(fake_velt(), checkout.join(format!("bin/velt{EXE}"))).unwrap();
    m.ok(
        &loose,
        &["toolchain", "link", "dev", checkout.to_str().unwrap()],
    );
    let stdout = m.ok_with(&pkg, &["build"], &[("VELT_TOOLCHAIN", "dev")]);
    assert!(same(&prefix_of(&stdout), &checkout));
    assert!(m.ok(&loose, &["toolchain", "list"]).contains("dev -> "));
    m.ok(&loose, &["toolchain", "unlink", "dev"]);
    let err = m.fail(&pkg, &["build"], &[("VELT_TOOLCHAIN", "dev")]);
    assert!(err.contains("no toolchain is linked as `dev`"), "{err}");
    assert!(checkout.join("bin").is_dir());

    let err = m.fail(&loose, &["toolchain", "frob"], &[]);
    assert!(
        err.contains("unknown command `velt toolchain frob`"),
        "{err}"
    );
    assert!(m
        .ok(&loose, &["toolchain"])
        .contains("Usage: velt toolchain"));
}

#[test]
fn the_launcher_must_be_in_a_bin_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let elsewhere = tmp.path().join(format!("velt{EXE}"));
    std::fs::copy(env!("CARGO_BIN_EXE_velt-launcher"), &elsewhere).unwrap();
    let out = Command::new(&elsewhere).arg("build").output().unwrap();
    let (_, stderr) = text(&out);
    assert!(!out.status.success());
    assert!(
        stderr.contains("must be installed as <root>/bin/velt"),
        "{stderr}"
    );
}

#[test]
fn yanked_releases_are_skipped_unless_named() {
    let releases = Releases::new(&["0.1.0", "0.1.1"]);
    releases.index(&[("0.1.0", None), ("0.1.1", Some("miscompiles closures"))]);
    let m = Machine::new(&releases);
    let pkg = m.package("app", Some("0.1"));
    let stdout = m.ok(&pkg, &["build"]);
    assert!(same(&prefix_of(&stdout), &m.toolchain("0.1.0")), "{stdout}");
    let available = m.ok(&m.work, &["toolchain", "list", "--available"]);
    assert_eq!(
        available,
        "0.1.1  (yanked: miscompiles closures)\n0.1.0  (installed)\n"
    );
    // Named exactly, it is still installed: the user chose it.
    let out = m.ok(&m.work, &["toolchain", "install", "=0.1.1"]);
    assert!(out.contains("installed velt 0.1.1"), "{out}");
    // A forged index is refused.
    releases.files.lock().unwrap().insert(
        "/releases/download/index/releases.json".into(),
        br#"{"format": 1, "releases": [{"version": "0.1.1"}]}"#.to_vec(),
    );
    let fresh = m.package("fresh", Some("=0.1.0"));
    std::fs::remove_dir_all(m.toolchain("0.1.0")).unwrap();
    // An exact pin doesn't need the index; a range does.
    m.ok(&fresh, &["build"]);
    let err = m.fail(&m.work, &["toolchain", "install", "0.1"], &[]);
    assert!(err.contains("does not match the release key"), "{err}");
}
