//! The launcher's tests against fake releases served from this machine: the releases
//! (`Releases`) and a root with the launcher in it (`Machine`). Shared by side_by_side.rs and
//! self_update.rs, each of which uses part of it.
#![allow(dead_code)]

use std::collections::HashMap;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};

use crate::support::fake_velt;
use velt_http::{Response, Server};

pub const EXE: &str = std::env::consts::EXE_SUFFIX;

pub fn host() -> String {
    velt_toolchain::release::host_triple()
}

/// `velt-<v>-<host>.tar.gz` holding the fake toolchain.
pub fn archive(version: &str) -> Vec<u8> {
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
    // The release's launcher: the fake toolchain stands in (a test sees it replace bin/velt).
    add(
        format!("{top}/bin/velt-launcher{EXE}"),
        &std::fs::read(fake_velt()).unwrap(),
        0o755,
    );
    add(format!("{top}/std/VERSION"), version.as_bytes(), 0o644);
    tar.into_inner().unwrap().finish().unwrap()
}

/// Releases served over loopback HTTP, laid out like GitHub's, signed with a key of their own.
pub struct Releases {
    pub server: Option<Server>,
    /// The public key, hex (`$VELT_INSTALL_PUBLIC_KEY`).
    pub key: String,
    pub signer: ring::signature::Ed25519KeyPair,
    /// The index's `generated` time, increased with each new index.
    pub generated: std::sync::atomic::AtomicU64,
    pub files: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    pub requests: Arc<Mutex<Vec<String>>>,
}

impl Releases {
    pub fn new(versions: &[&str]) -> Releases {
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
            generated: std::sync::atomic::AtomicU64::new(1_000),
            files,
            requests,
        };
        let listed: Vec<(&str, Option<&str>)> = versions.iter().map(|v| (*v, None)).collect();
        releases.index(&listed);
        releases
    }

    /// Publish the signed index of releases: (version, why it was yanked).
    pub fn index(&self, releases: &[(&str, Option<&str>)]) {
        self.index_with(releases, None, None);
    }

    /// [`Releases::index`] naming the newest launcher, and written at `generated` (default:
    /// later than the last one).
    pub fn index_with(
        &self,
        releases: &[(&str, Option<&str>)],
        launcher: Option<&str>,
        generated: Option<u64>,
    ) {
        use std::sync::atomic::Ordering;
        let generated =
            generated.unwrap_or_else(|| self.generated.fetch_add(1, Ordering::Relaxed) + 1);
        let launcher = launcher.map_or(String::new(), |l| format!("\"launcher\": \"{l}\", "));
        let entries: Vec<String> = releases
            .iter()
            .map(|(v, yanked)| match yanked {
                Some(why) => format!("{{\"version\": \"{v}\", \"yanked\": \"{why}\"}}"),
                None => format!("{{\"version\": \"{v}\"}}"),
            })
            .collect();
        let index = format!(
            "{{\"format\": 1, \"generated\": {generated}, {launcher}\"releases\": [{}]}}",
            entries.join(", ")
        );
        let path = "/releases/download/index/releases.json";
        let mut files = self.files.lock().unwrap();
        files.insert(
            format!("{path}.sig"),
            self.signer.sign(index.as_bytes()).as_ref().to_vec(),
        );
        files.insert(path.into(), index.into_bytes());
    }

    pub fn url(&self) -> String {
        format!("http://{}", self.server.as_ref().unwrap().addr())
    }

    pub fn downloads(&self) -> usize {
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
pub struct Machine {
    pub _tmp: tempfile::TempDir,
    pub root: PathBuf,
    pub work: PathBuf,
    pub base: String,
    pub key: String,
}

impl Machine {
    pub fn new(releases: &Releases) -> Machine {
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
    pub fn package(&self, name: &str, velt: Option<&str>) -> PathBuf {
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

    pub fn command(&self, dir: &Path, args: &[&str]) -> Command {
        let mut c = Command::new(self.root.join(format!("bin/velt{EXE}")));
        c.args(args)
            .current_dir(dir)
            .env("VELT_INSTALL_BASE_URL", &self.base)
            .env("VELT_INSTALL_PUBLIC_KEY", &self.key)
            .env_remove("VELT_TOOLCHAIN")
            .env_remove("VELT_TOOLCHAIN_AUTO_INSTALL");
        c
    }

    pub fn run(&self, dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
        let mut c = self.command(dir, args);
        for (k, v) in env {
            c.env(k, v);
        }
        c.output().unwrap()
    }

    pub fn ok(&self, dir: &Path, args: &[&str]) -> String {
        self.ok_with(dir, args, &[])
    }

    pub fn ok_with(&self, dir: &Path, args: &[&str], env: &[(&str, &str)]) -> String {
        let out = self.run(dir, args, env);
        let (stdout, stderr) = text(&out);
        assert!(
            out.status.success(),
            "velt {args:?} failed:\n{stdout}\n{stderr}"
        );
        stdout
    }

    pub fn fail(&self, dir: &Path, args: &[&str], env: &[(&str, &str)]) -> String {
        let out = self.run(dir, args, env);
        let (stdout, stderr) = text(&out);
        assert!(!out.status.success(), "velt {args:?} succeeded:\n{stdout}");
        stderr
    }

    pub fn toolchain(&self, version: &str) -> PathBuf {
        self.root.join("toolchains").join(version)
    }
}

pub fn text(out: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The `prefix=` line the fake toolchain prints.
pub fn prefix_of(stdout: &str) -> PathBuf {
    let line = stdout
        .lines()
        .find_map(|l| l.strip_prefix("prefix="))
        .unwrap_or_else(|| panic!("not run by a toolchain:\n{stdout}"));
    PathBuf::from(line)
}

pub fn same(a: &Path, b: &Path) -> bool {
    a.canonicalize().unwrap() == b.canonicalize().unwrap()
}
