//! scripts/get-velt.sh (and get-velt.ps1 on Windows) against fake releases (#948): versions
//! install side by side under `<root>/toolchains/`, the launcher goes into
//! `<root>/bin`, the first version becomes the default, and a download is checked against the
//! signed SHA256SUMS.

mod support;

#[cfg(unix)]
use std::collections::HashMap;
#[cfg(unix)]
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
#[cfg(unix)]
use std::sync::Arc;

use support::fake_velt;

const EXE: &str = std::env::consts::EXE_SUFFIX;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn host() -> String {
    velt_toolchain::release::host_triple()
}

/// `velt-<version>-<host>.tar.gz` like a release's: the fake toolchain, the launcher (unless
/// `launcher` is false) and `std/VERSION`.
#[cfg(unix)]
fn archive(version: &str, launcher: bool) -> Vec<u8> {
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
    if launcher {
        let bytes = std::fs::read(env!("CARGO_BIN_EXE_velt-launcher")).unwrap();
        add(format!("{top}/bin/velt-launcher{EXE}"), &bytes, 0o755);
    }
    add(format!("{top}/std/VERSION"), version.as_bytes(), 0o644);
    tar.into_inner().unwrap().finish().unwrap()
}

/// get-velt.sh with `args`, HOME in `home` (so no profile of the machine's is touched).
#[cfg(unix)]
fn get_velt(home: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut c = Command::new("sh");
    c.arg(repo().join("scripts/get-velt.sh"))
        .args(args)
        .env("HOME", home)
        .env_remove("VELT_INSTALL_PREFIX")
        .env_remove("VELT_INSTALL_VERSION")
        .env_remove("VELT_INSTALL_BASE_URL")
        .env_remove("VELT_INSTALL_PUBLIC_KEY")
        .env_remove("VELT_TOOLCHAIN")
        .current_dir(home);
    for (k, v) in env {
        c.env(k, v);
    }
    c.output().unwrap()
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn ok(out: Output) -> String {
    let all = text(&out);
    assert!(out.status.success(), "failed:\n{all}");
    all
}

fn velt(root: &Path, dir: &Path, args: &[&str]) -> String {
    let out = Command::new(root.join(format!("bin/velt{EXE}")))
        .args(args)
        .current_dir(dir)
        .env_remove("VELT_TOOLCHAIN")
        .output()
        .unwrap();
    ok(out)
}

#[cfg(unix)]
#[test]
fn get_velt_sh_installs_versions_side_by_side() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let write = |name: &str, bytes: Vec<u8>| {
        let path = tmp.path().join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    };
    let a = write("a.tar.gz", archive("0.1.1", true));
    let b = write("b.tar.gz", archive("0.2.0", true));
    let root = home.join(".velt");
    let install = |archive: &Path, extra: &[&str]| {
        let mut args = vec!["--no-modify-path", "--archive", archive.to_str().unwrap()];
        args.extend_from_slice(extra);
        get_velt(&home, &args, &[])
    };

    let out = ok(install(&a, &[]));
    assert!(out.contains("velt 0.1.1 is the default"), "{out}");
    assert!(root.join("toolchains/0.1.1/bin/velt").is_file());
    assert!(root.join("toolchains/0.1.1/std/VERSION").is_file());
    assert_eq!(
        std::fs::read_to_string(root.join("default")).unwrap(),
        "0.1.1\n"
    );
    let version = velt(&root, &home, &["--version"]);
    assert!(version.starts_with("velt 0.1.1 "), "{version}");
    let launcher = velt(&root, &home, &["toolchain", "--version"]);
    assert!(launcher.starts_with("velt-launcher "), "{launcher}");

    // Another version goes beside it; the default stays.
    let out = ok(install(&b, &[]));
    assert!(!out.contains("is the default"), "{out}");
    let list = velt(&root, &home, &["toolchain", "list"]);
    // The default (`*`) is also what the home directory, outside a package, selects (`>`).
    assert_eq!(list, "   0.2.0\n*> 0.1.1\n");
    // Again: already there, kept (unless --force).
    let out = ok(install(&a, &[]));
    assert!(out.contains("velt 0.1.1 is already installed"), "{out}");
    let out = ok(install(&a, &["--force"]));
    assert!(out.contains("installed velt 0.1.1"), "{out}");
    // --default switches it.
    ok(install(&b, &["--default"]));
    assert_eq!(
        std::fs::read_to_string(root.join("default")).unwrap(),
        "0.2.0\n"
    );
    // Nothing staged or renamed aside is left.
    let hidden: Vec<_> = std::fs::read_dir(root.join("toolchains"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with('.'))
        .collect();
    assert!(hidden.is_empty(), "{hidden:?}");

    // A release from before the launcher is refused.
    let old = write("old.tar.gz", archive("0.1.0", false));
    let out = install(&old, &[]);
    assert!(!out.status.success());
    assert!(text(&out).contains("has no launcher"), "{}", text(&out));

    // PATH: profiles get <root>/bin.
    ok(get_velt(&home, &["--archive", a.to_str().unwrap()], &[]));
    let profile = std::fs::read_to_string(home.join(".profile")).unwrap();
    assert!(
        profile.contains(&format!("{}/bin", root.display())),
        "{profile}"
    );
}

/// The download path: SHA256SUMS checked, and its signature when OpenSSL checks Ed25519.
#[cfg(unix)]
#[test]
fn get_velt_sh_checks_a_download_against_the_signed_sums() {
    let has_ed25519 = Command::new("sh")
        .args(["-c", "openssl pkeyutl -help 2>&1 | grep -q -- -rawin"])
        .status()
        .is_ok_and(|s| s.success());
    use ring::signature::KeyPair;
    let pkcs8 =
        ring::signature::Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new()).unwrap();
    let signer = ring::signature::Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
    let key: String = signer
        .public_key()
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let (forger_pkcs8, version) = (
        ring::signature::Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new()).unwrap(),
        "0.3.0",
    );
    let forger = ring::signature::Ed25519KeyPair::from_pkcs8(forger_pkcs8.as_ref()).unwrap();
    let name = format!("velt-{version}-{}.tar.gz", host());
    let bytes = archive(version, true);
    let sums = format!("{}  {name}\n", velt_toolchain::install::sha256_hex(&bytes));
    let serve = |sig: Vec<u8>| {
        let dir = format!("/releases/download/v{version}");
        let files: HashMap<String, Vec<u8>> = [
            (format!("{dir}/{name}"), bytes.clone()),
            (format!("{dir}/SHA256SUMS"), sums.clone().into_bytes()),
            (format!("{dir}/SHA256SUMS.sig"), sig),
        ]
        .into_iter()
        .collect();
        let handler = Arc::new(move |req: velt_http::Request| match files.get(&req.path) {
            Some(b) => velt_http::Response::bytes(200, "application/octet-stream", b.clone()),
            None => velt_http::Response::text(404, "not found"),
        });
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        velt_http::Server::start(listener, handler, 1 << 20).unwrap()
    };
    let tmp = tempfile::tempdir().unwrap();
    let run = |server: &velt_http::Server, home: &Path| {
        let base = format!("http://{}", server.addr());
        get_velt(
            home,
            &["--no-modify-path", "--version", version],
            &[
                ("VELT_INSTALL_BASE_URL", &base),
                ("VELT_INSTALL_PUBLIC_KEY", &key),
            ],
        )
    };

    let home = tmp.path().join("good");
    std::fs::create_dir_all(&home).unwrap();
    let good = serve(signer.sign(sums.as_bytes()).as_ref().to_vec());
    let out = ok(run(&good, &home));
    good.stop();
    if has_ed25519 {
        assert!(out.contains("checked the signature of SHA256SUMS"), "{out}");
    } else {
        assert!(out.contains("only the checksum was checked"), "{out}");
    }
    assert!(home.join(".velt/toolchains/0.3.0/bin/velt").is_file());

    if has_ed25519 {
        let home = tmp.path().join("forged");
        std::fs::create_dir_all(&home).unwrap();
        let forged = serve(forger.sign(sums.as_bytes()).as_ref().to_vec());
        let out = run(&forged, &home);
        forged.stop();
        assert!(!out.status.success());
        assert!(
            text(&out).contains("does not match its signature"),
            "{}",
            text(&out)
        );
        assert!(!home.join(".velt/toolchains/0.3.0").exists());
    }
}

/// get-velt.ps1 on Windows, the only OS it installs for (PowerShell on Linux or macOS can run it,
/// but it installs `velt.exe`).
#[cfg(windows)]
#[test]
fn get_velt_ps1_installs_versions_side_by_side() {
    let Ok(probe) = Command::new("pwsh")
        .args(["-NoProfile", "-Command", "1"])
        .output()
    else {
        eprintln!("skipping: no pwsh");
        return;
    };
    assert!(probe.status.success());
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let zip = |version: &str| {
        // The Windows installer takes the release's .zip.
        let dir = tmp.path().join(format!("src-{version}"));
        let top = dir.join(format!("velt-{version}-{}", host()));
        std::fs::create_dir_all(top.join("bin")).unwrap();
        std::fs::create_dir_all(top.join("std")).unwrap();
        std::fs::copy(fake_velt(), top.join(format!("bin/velt{EXE}"))).unwrap();
        std::fs::copy(
            env!("CARGO_BIN_EXE_velt-launcher"),
            top.join(format!("bin/velt-launcher{EXE}")),
        )
        .unwrap();
        std::fs::write(top.join("std/VERSION"), version).unwrap();
        let zip = tmp.path().join(format!("velt-{version}.zip"));
        let script = format!(
            "Compress-Archive -Path '{}' -DestinationPath '{}'",
            top.display(),
            zip.display()
        );
        ok(Command::new("pwsh")
            .args(["-NoProfile", "-Command", &script])
            .output()
            .unwrap());
        zip
    };
    let install = |zip: &Path, extra: &[&str]| {
        let mut c = Command::new("pwsh");
        c.args(["-NoProfile", "-File"])
            .arg(repo().join("scripts/get-velt.ps1"))
            .args(["-NoModifyPath", "-Prefix"])
            .arg(&root)
            .arg("-Archive")
            .arg(zip)
            .args(extra)
            .env_remove("VELT_TOOLCHAIN");
        c.output().unwrap()
    };
    // An install from before the launcher: the compiler in bin\, beside lib\ and std\. It
    // moves into toolchains\<its version>; the fake compiler answers `toolchain --version`
    // with something else than a launcher's version, so it is replaced by the launcher.
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::fs::create_dir_all(root.join("lib")).unwrap();
    std::fs::create_dir_all(root.join("std")).unwrap();
    std::fs::copy(fake_velt(), root.join("bin/velt.exe")).unwrap();
    std::fs::write(root.join("std/VERSION"), "0.1.0").unwrap();
    std::fs::write(root.join("README.md"), "old").unwrap();
    let a = zip("0.1.1");
    let b = zip("0.2.0");
    let out = ok(install(&a, &[]));
    assert!(
        out.contains("moving the earlier install of velt 0.1.0"),
        "{out}"
    );
    assert!(out.contains("velt 0.1.1 is the default"), "{out}");
    assert!(root.join("toolchains/0.1.0/bin/velt.exe").is_file());
    assert!(root.join("toolchains/0.1.0/std/VERSION").is_file());
    assert!(root.join("toolchains/0.1.0/README.md").is_file());
    assert!(!root.join("std").exists() && !root.join("lib").exists());
    let old = velt(&root, tmp.path(), &["+0.1.0", "--version"]);
    assert!(old.starts_with("velt 0.1.0 "), "{old}");
    ok(install(&b, &[]));
    assert!(root
        .join(format!("toolchains/0.1.1/bin/velt{EXE}"))
        .is_file());
    assert!(root
        .join(format!("toolchains/0.2.0/bin/velt{EXE}"))
        .is_file());
    assert_eq!(
        std::fs::read_to_string(root.join("default"))
            .unwrap()
            .trim(),
        "0.1.1"
    );
    let version = velt(&root, tmp.path(), &["--version"]);
    assert!(version.starts_with("velt 0.1.1 "), "{version}");
    ok(install(&b, &["-Default"]));
    assert_eq!(
        std::fs::read_to_string(root.join("default"))
            .unwrap()
            .trim(),
        "0.2.0"
    );
}

/// scripts/release-index.sh writes the index `velt_toolchain::release::parse_index` reads.
#[cfg(unix)]
#[test]
fn release_index_sh_writes_what_the_launcher_reads() {
    if Command::new("jq").arg("--version").output().is_err() {
        eprintln!("skipping: no jq");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let releases = tmp.path().join("releases");
    let signed = r#"["velt-x.tar.gz", "SHA256SUMS", "SHA256SUMS.sig"]"#;
    std::fs::write(
        &releases,
        [
            // Before signing: the launcher cannot install it.
            r#"{"tag": "v0.1.0", "draft": false, "assets": ["SHA256SUMS"]}"#.to_string(),
            format!(r#"{{"tag": "v0.1.1", "draft": false, "assets": {signed}}}"#),
            format!(r#"{{"tag": "v0.1.10", "draft": false, "assets": {signed}}}"#),
            format!(r#"{{"tag": "v0.1.9", "draft": false, "assets": {signed}}}"#),
            format!(r#"{{"tag": "v0.2.0-rc.1", "draft": false, "assets": {signed}}}"#),
            format!(r#"{{"tag": "v0.3.0", "draft": true, "assets": {signed}}}"#),
            r#"{"tag": "index", "draft": false, "assets": ["releases.json"]}"#.to_string(),
        ]
        .join("\n"),
    )
    .unwrap();
    let yanks = tmp.path().join("yanks.json");
    std::fs::write(
        &yanks,
        r#"{"0.1.10": "miscompiles \"closures\"; use 0.1.11"}"#,
    )
    .unwrap();
    let out = Command::new("sh")
        .arg(repo().join("scripts/release-index.sh"))
        .arg(&releases)
        .arg(&yanks)
        .arg("1700000000")
        .output()
        .unwrap();
    let json = ok(out);
    let index = velt_toolchain::release::parse_index(&json).unwrap();
    assert_eq!(index.generated, 1_700_000_000);
    // The newest stable release that is not yanked: 0.1.10 is newer than 0.1.9 but yanked.
    assert_eq!(index.launcher.unwrap().to_string(), "0.1.9");
    let listed: Vec<(String, Option<String>)> = index
        .releases
        .iter()
        .map(|r| (r.version.to_string(), r.yanked.clone()))
        .collect();
    assert_eq!(
        listed,
        [
            ("0.1.1".to_string(), None),
            ("0.1.9".to_string(), None),
            (
                "0.1.10".to_string(),
                Some("miscompiles \"closures\"; use 0.1.11".to_string())
            ),
            ("0.2.0-rc.1".to_string(), None),
        ]
    );
}

/// The key get-velt.sh checks releases with is the one velt is built with
/// (velt_toolchain::signature::RELEASE_PUBLIC_KEY), as scripts/release-sign.sh derives it.
#[cfg(unix)]
#[test]
fn the_installer_and_the_signing_script_use_the_built_in_key() {
    if Command::new("openssl").arg("version").output().is_err() {
        eprintln!("skipping: no openssl");
        return;
    }
    let out = Command::new("sh")
        .arg(repo().join("scripts/release-sign.sh"))
        .arg("--public-key")
        .output()
        .unwrap();
    let pem = ok(out);
    let base64 = pem.lines().nth(1).unwrap();
    let installer = std::fs::read_to_string(repo().join("scripts/get-velt.sh")).unwrap();
    assert!(
        installer.contains(base64),
        "get-velt.sh does not embed {base64}"
    );
    // The DER is a fixed header and the raw key, which RELEASE_PUBLIC_KEY holds in hex.
    let tail = velt_toolchain::signature::RELEASE_PUBLIC_KEY;
    assert_eq!(tail.len(), 64);
    let der = ok(Command::new("sh")
        .arg("-c")
        .arg(format!(
            "printf '%s\\n' '{base64}' | openssl base64 -d -A | od -An -v -tx1 | tr -d ' \\n'"
        ))
        .output()
        .unwrap());
    assert_eq!(der, format!("302a300506032b6570032100{tail}"));
}

/// scripts/install.sh puts a dist directory beside the installed versions, replacing a copy of
/// the same version (a rebuild), with its launcher.
#[cfg(unix)]
#[test]
fn install_sh_installs_a_dist_directory_as_a_version() {
    let tmp = tempfile::tempdir().unwrap();
    let dist = |version: &str| {
        let dir = tmp.path().join(format!("dist-{version}"));
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::create_dir_all(dir.join("lib")).unwrap();
        std::fs::create_dir_all(dir.join("std")).unwrap();
        std::fs::copy(fake_velt(), dir.join("bin/velt")).unwrap();
        std::fs::copy(
            env!("CARGO_BIN_EXE_velt-launcher"),
            dir.join("bin/velt-launcher"),
        )
        .unwrap();
        std::fs::write(dir.join("lib/libvelt_rt.a"), b"!<arch>\n").unwrap();
        std::fs::write(dir.join("std/VERSION"), version).unwrap();
        dir
    };
    let root = tmp.path().join("root");
    let install = |dir: &Path| {
        ok(Command::new("sh")
            .arg(repo().join("scripts/install.sh"))
            .arg(dir)
            .arg(&root)
            .output()
            .unwrap())
    };
    let a = dist("0.1.1");
    install(&a);
    std::fs::write(a.join("std/REBUILT"), b"").unwrap();
    install(&a);
    assert!(root.join("toolchains/0.1.1/std/REBUILT").is_file());
    install(&dist("0.2.0"));
    assert_eq!(
        std::fs::read_to_string(root.join("default")).unwrap(),
        "0.1.1\n"
    );
    let version = velt(&root, tmp.path(), &["--version"]);
    assert!(version.starts_with("velt 0.1.1 "), "{version}");
    let list = velt(&root, tmp.path(), &["toolchain", "list"]);
    assert!(list.contains("0.2.0") && list.contains("0.1.1"), "{list}");
}

/// The launcher in `<root>/bin` is replaced unless it is a newer velt's: a pre-release is older
/// than its release, and anything that doesn't answer `velt-launcher <version>` is replaced.
#[cfg(unix)]
#[test]
fn get_velt_sh_replaces_the_launcher_only_with_a_newer_one() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let bin = home.join(".velt/bin");
    std::fs::create_dir_all(&bin).unwrap();
    // A launcher that says it is `<says>`, and runs the real one (from the same bin/) otherwise.
    std::fs::copy(env!("CARGO_BIN_EXE_velt-launcher"), bin.join("real")).unwrap();
    let pretend = |says: &str| {
        let script = format!(
            "#!/bin/sh\nif [ \"$1 $2\" = \"toolchain --version\" ]; then echo '{says}'; exit 0; fi\n\
             exec \"$(dirname \"$0\")/real\" \"$@\"\n"
        );
        std::fs::write(bin.join("velt"), script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(bin.join("velt"), std::fs::Permissions::from_mode(0o755)).unwrap();
    };
    let is_pretend = || {
        std::fs::read(bin.join("velt"))
            .unwrap()
            .starts_with(b"#!/bin/sh")
    };
    let install = |version: &str| {
        let file = tmp.path().join(format!("{version}.tar.gz"));
        std::fs::write(&file, archive(version, true)).unwrap();
        ok(get_velt(
            &home,
            &["--no-modify-path", "--archive", file.to_str().unwrap()],
            &[],
        ))
    };

    pretend("velt-launcher 0.2.0");
    install("0.2.0-rc.1");
    assert!(
        is_pretend(),
        "a pre-release replaced its release's launcher"
    );
    install("0.1.9");
    assert!(is_pretend(), "an older version replaced the launcher");
    install("0.2.0");
    assert!(
        !is_pretend(),
        "the same version did not replace the launcher"
    );

    pretend("velt 0.9.0 (abc x86_64-apple-darwin)");
    install("0.1.8");
    assert!(!is_pretend(), "something that is not a launcher was kept");
    pretend("velt-launcher 0.3.0-rc.1");
    install("0.3.0");
    assert!(
        !is_pretend(),
        "a release did not replace its pre-release's launcher"
    );
}

/// The base URL is https, or http to this machine; nothing else is downloaded from.
#[cfg(unix)]
#[test]
fn get_velt_sh_downloads_only_over_https_or_from_this_machine() {
    let tmp = tempfile::tempdir().unwrap();
    for base in [
        "http://releases.example",
        "file:///tmp/releases",
        "ftp://x.example",
    ] {
        let out = get_velt(
            tmp.path(),
            &["--version", "0.3.0"],
            &[("VELT_INSTALL_BASE_URL", base)],
        );
        assert!(!out.status.success());
        assert!(
            text(&out).contains("VELT_INSTALL_BASE_URL must be an https:// URL"),
            "{base}: {}",
            text(&out)
        );
    }
}

/// musl is the C library `ldd` reports, not a musl loader installed beside glibc.
#[cfg(target_os = "linux")]
#[test]
fn get_velt_sh_asks_the_c_library_whether_it_is_musl() {
    let tmp = tempfile::tempdir().unwrap();
    let fake_bin = tmp.path().join("fake-bin");
    std::fs::create_dir_all(&fake_bin).unwrap();
    let path = format!("{}:{}", fake_bin.display(), std::env::var("PATH").unwrap());
    let with_ldd = |says: &str| {
        use std::os::unix::fs::PermissionsExt;
        let ldd = fake_bin.join("ldd");
        std::fs::write(&ldd, format!("#!/bin/sh\necho '{says}' >&2\n")).unwrap();
        std::fs::set_permissions(&ldd, std::fs::Permissions::from_mode(0o755)).unwrap();
        // A closed port: a glibc machine gets as far as the download.
        get_velt(
            tmp.path(),
            &["--no-modify-path", "--version", "0.3.0"],
            &[
                ("PATH", &path),
                ("VELT_INSTALL_BASE_URL", "http://127.0.0.1:9"),
            ],
        )
    };
    let musl = text(&with_ldd("musl libc (x86_64)"));
    assert!(musl.contains("no prebuilt Velt for musl"), "{musl}");
    let glibc = text(&with_ldd("ldd (Debian GLIBC 2.36-9) 2.36"));
    assert!(
        !glibc.contains("musl") && glibc.contains("download failed"),
        "{glibc}"
    );
}
