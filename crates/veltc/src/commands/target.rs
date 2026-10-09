//! `velt target list|add|remove`: target packs, what `velt build --target <triple>` needs to
//! build for a platform other than this one (#856). A pack is `velt-<version>-target-<triple>.tar.gz`,
//! a release asset holding `<triple>/`: the target's runtime library and its link kit
//! (`velt_link::kit`). It is installed into `<prefix>/lib/targets/<triple>/`, where `velt_link`
//! looks for both.

use std::io::Read;
use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::cli::target::TargetAction;

/// Where releases are downloaded from (`$VELT_INSTALL_BASE_URL`, as for the installers).
const DEFAULT_BASE_URL: &str = "https://github.com/velt-lang/velt";

pub fn target_command(action: &TargetAction) -> Result<(), String> {
    match action {
        TargetAction::List => list(),
        TargetAction::Add { targets, from } => {
            for target in targets {
                add(target, from.as_deref())?;
            }
            Ok(())
        }
        TargetAction::Remove { targets } => {
            for target in targets {
                remove(target)?;
            }
            Ok(())
        }
    }
}

fn targets_dir() -> Result<PathBuf, String> {
    velt_link::kit::targets_dir().ok_or_else(|| "cannot find this toolchain's directory".into())
}

fn list() -> Result<(), String> {
    let host = velt_link::host_triple();
    let dir = targets_dir()?;
    println!("{host}  (this machine)");
    let mut shown = vec![host.clone()];
    let mut installed: Vec<String> = std::fs::read_dir(&dir)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter(|e| e.path().join(velt_link::kit::STAMP).is_file())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    installed.sort();
    for t in &installed {
        if velt_link::same_target(t, &host) {
            continue;
        }
        let state = match pack_problem(&dir.join(t), t) {
            None => "installed".to_string(),
            Some(why) => format!("broken: {why}; run `velt target add {t}` again"),
        };
        println!("{t}  ({state})");
        shown.push(t.clone());
    }
    for t in velt_link::kit::RELEASE_TARGETS {
        if !shown.iter().any(|s| velt_link::same_target(s, t)) {
            println!("{t}  (not installed: velt target add {t})");
        }
    }
    Ok(())
}

/// What is wrong with the pack in `dir` for `target`, if anything: the kit must be complete
/// and the runtime library there.
fn pack_problem(dir: &Path, target: &str) -> Option<String> {
    if let Err(e) = velt_link::kit::Kit::open(dir, target) {
        return Some(e);
    }
    let runtime = dir.join(velt_link::runtime_lib_name(target));
    (!runtime.is_file()).then(|| format!("{} is missing", runtime.display()))
}

fn add(target: &str, from: Option<&Path>) -> Result<(), String> {
    if !velt_link::kit::RELEASE_TARGETS.contains(&target) {
        return Err(format!(
            "no target pack exists for `{target}`; the targets are: {}",
            velt_link::kit::RELEASE_TARGETS.join(", ")
        ));
    }
    if velt_link::same_target(target, &velt_link::host_triple()) {
        println!("{target} is this machine: the toolchain builds for it already");
        return Ok(());
    }
    let name = pack_name(target);
    let archive = match from {
        Some(file) => {
            let bytes =
                std::fs::read(file).map_err(|e| format!("cannot read {}: {e}", file.display()))?;
            // A pack downloaded by hand is checked against a SHA256SUMS beside it, if any.
            let sums = file.with_file_name("SHA256SUMS");
            if let Ok(text) = std::fs::read_to_string(&sums) {
                let file_name = file.file_name().unwrap_or_default().to_string_lossy();
                check_sha256(&bytes, &text, &file_name)?;
            }
            bytes
        }
        None => {
            let base = std::env::var("VELT_INSTALL_BASE_URL")
                .ok()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| DEFAULT_BASE_URL.into());
            let release = format!(
                "{}/releases/download/v{}",
                base.trim_end_matches('/'),
                env!("CARGO_PKG_VERSION")
            );
            eprintln!("downloading {name}...");
            let bytes = download(&format!("{release}/{name}"))?;
            let sums = download(&format!("{release}/SHA256SUMS"))?;
            check_sha256(&bytes, &String::from_utf8_lossy(&sums), &name)?;
            bytes
        }
    };
    let dir = targets_dir()?;
    install(&archive, target, &dir)?;
    println!(
        "installed {target} into {}; build with `velt build --target {target}`",
        dir.join(target).display()
    );
    Ok(())
}

fn pack_name(target: &str) -> String {
    format!("velt-{}-target-{target}.tar.gz", env!("CARGO_PKG_VERSION"))
}

/// GET `url`, following redirects (release downloads redirect to a storage host), over
/// `https://` only (or `http://` to this machine).
fn download(url: &str) -> Result<Vec<u8>, String> {
    let mut url = url.to_string();
    for _ in 0..5 {
        if !vpm::remote::is_tls_or_loopback(&url) {
            return Err(format!("refusing to download over plain http: {url}"));
        }
        let response = velt_http::fetch("GET", &url, &[("User-Agent", "velt")], &[])
            .map_err(|e| format!("cannot download {url}: {e}"))?;
        match response.status {
            200 => return Ok(response.body),
            301 | 302 | 303 | 307 | 308 => {
                url = response
                    .header("Location")
                    .ok_or_else(|| format!("{url}: a redirect without a Location"))?
                    .to_string();
            }
            404 => {
                return Err(format!(
                    "{url} does not exist (HTTP 404): this velt's release ({}) has no such \
                     target pack; a toolchain built from source installs packs with `--from`",
                    env!("CARGO_PKG_VERSION")
                ))
            }
            status => return Err(format!("cannot download {url}: HTTP {status}")),
        }
    }
    Err(format!("too many redirects downloading {url}"))
}

/// Check `bytes` against the line for `name` in a `SHA256SUMS` file.
fn check_sha256(bytes: &[u8], sums: &str, name: &str) -> Result<(), String> {
    let expected = sums
        .lines()
        .find_map(|line| {
            let (hash, file) = line.split_once(char::is_whitespace)?;
            (file.trim().trim_start_matches('*') == name).then(|| hash.to_ascii_lowercase())
        })
        .ok_or_else(|| format!("SHA256SUMS has no entry for {name}"))?;
    let actual: String = Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if actual != expected {
        return Err(format!(
            "{name} has SHA-256 {actual}, but SHA256SUMS says {expected}"
        ));
    }
    Ok(())
}

/// Unpack a pack (a `.tar.gz` of `<target>/...`) into `<dir>/<target>`, replacing an installed
/// one only once the new one is complete.
fn install(archive: &[u8], target: &str, dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let staging = dir.join(format!(".{target}.{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    let result =
        unpack(archive, target, &staging).and_then(|()| match pack_problem(&staging, target) {
            Some(why) => Err(format!("the target pack for {target} is incomplete: {why}")),
            None => Ok(()),
        });
    if let Err(e) = result {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(e);
    }
    let dest = dir.join(target);
    let _ = std::fs::remove_dir_all(&dest);
    std::fs::rename(&staging, &dest).map_err(|e| {
        let _ = std::fs::remove_dir_all(&staging);
        format!("cannot install into {}: {e}", dest.display())
    })
}

/// Extract the regular files under `<target>/` into `into`; any other path (absolute, `..`,
/// another top directory) or entry type (links, devices) is refused.
fn unpack(archive: &[u8], target: &str, into: &Path) -> Result<(), String> {
    let gz = flate2::read::GzDecoder::new(archive);
    let mut tar = tar::Archive::new(gz);
    let entries = tar
        .entries()
        .map_err(|e| format!("not a target pack (.tar.gz): {e}"))?;
    let bad = |path: &Path, why: &str| format!("the target pack holds {}: {why}", path.display());
    for entry in entries {
        let mut entry = entry.map_err(|e| format!("cannot read the target pack: {e}"))?;
        let path = entry
            .path()
            .map_err(|e| format!("cannot read the target pack: {e}"))?
            .into_owned();
        // macOS `tar` adds AppleDouble metadata (`._<name>`) beside each file; it is not ours.
        if path
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with("._"))
        {
            continue;
        }
        let mut parts = path.components();
        if parts.next() != Some(Component::Normal(target.as_ref())) {
            return Err(bad(&path, &format!("everything must be under {target}/")));
        }
        let rel: PathBuf = parts.collect();
        if rel.components().any(|c| !matches!(c, Component::Normal(_))) {
            return Err(bad(&path, "not a plain relative path"));
        }
        let kind = entry.header().entry_type();
        if kind.is_dir() {
            continue;
        }
        if !kind.is_file() {
            return Err(bad(&path, "only files may be in a target pack"));
        }
        let out = into.join(&rel);
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
        let mut bytes = vec![];
        entry
            .read_to_end(&mut bytes)
            .map_err(|e| format!("cannot read {} from the target pack: {e}", path.display()))?;
        std::fs::write(&out, bytes).map_err(|e| format!("cannot write {}: {e}", out.display()))?;
    }
    Ok(())
}

fn remove(target: &str) -> Result<(), String> {
    if velt_link::same_target(target, &velt_link::host_triple()) {
        return Err(format!(
            "`{target}` is this machine: it is part of the toolchain"
        ));
    }
    let dir = targets_dir()?.join(target);
    if !dir.is_dir() {
        return Err(format!("`{target}` is not installed (`velt target list`)"));
    }
    std::fs::remove_dir_all(&dir).map_err(|e| format!("cannot remove {}: {e}", dir.display()))?;
    println!("removed {target}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `.tar.gz` holding `files` (path, contents).
    fn pack(files: &[(&str, &[u8])]) -> Vec<u8> {
        let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let mut tar = tar::Builder::new(gz);
        for (path, bytes) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            // The name goes in as given (`set_path` refuses `..`, which a hostile pack can hold).
            header.as_old_mut().name[..path.len()].copy_from_slice(path.as_bytes());
            header.set_cksum();
            tar.append(&header, *bytes).unwrap();
        }
        tar.into_inner().unwrap().finish().unwrap()
    }

    fn macos_pack(target: &str) -> Vec<(String, Vec<u8>)> {
        let mut files: Vec<(String, Vec<u8>)> = velt_link::kit::MACOS_LIBS
            .iter()
            .map(|(_, path, _)| (format!("{target}/{path}"), b"--- !tapi-tbd\n".to_vec()))
            .collect();
        files.push((
            format!("{target}/{}", velt_link::kit::STAMP),
            velt_link::kit::stamp_text(target).into_bytes(),
        ));
        files.push((format!("{target}/libvelt_rt.a"), b"!<arch>\n".to_vec()));
        files
    }

    fn bytes(files: &[(String, Vec<u8>)]) -> Vec<u8> {
        let refs: Vec<(&str, &[u8])> = files
            .iter()
            .map(|(p, b)| (p.as_str(), b.as_slice()))
            .collect();
        pack(&refs)
    }

    #[test]
    fn installs_a_complete_pack_and_replaces_the_old_one() {
        let tmp = tempfile::tempdir().unwrap();
        let target = "x86_64-apple-darwin";
        let archive = bytes(&macos_pack(target));
        install(&archive, target, tmp.path()).unwrap();
        let dir = tmp.path().join(target);
        assert_eq!(pack_problem(&dir, target), None);
        std::fs::write(dir.join("stale"), b"").unwrap();
        install(&archive, target, tmp.path()).unwrap();
        assert!(!dir.join("stale").exists());
    }

    #[test]
    fn refuses_incomplete_and_unsafe_packs() {
        let tmp = tempfile::tempdir().unwrap();
        let target = "x86_64-apple-darwin";
        let mut files = macos_pack(target);
        files.pop(); // no runtime
        let err = install(&bytes(&files), target, tmp.path()).unwrap_err();
        assert!(
            err.contains("incomplete") && err.contains("libvelt_rt.a"),
            "{err}"
        );
        assert!(!tmp.path().join(target).exists());

        let other = pack(&[("aarch64-apple-darwin/kit.stamp", b"")]);
        let err = install(&other, target, tmp.path()).unwrap_err();
        assert!(err.contains("under x86_64-apple-darwin/"), "{err}");

        let escape = pack(&[("x86_64-apple-darwin/../../evil", b"")]);
        assert!(install(&escape, target, tmp.path()).is_err());
        assert!(!tmp.path().join("evil").exists());

        assert!(install(b"not gzip", target, tmp.path()).is_err());
        // macOS metadata entries are skipped, not refused.
        let mut files = macos_pack(target);
        files.push(("._x86_64-apple-darwin".into(), b"meta".to_vec()));
        install(&bytes(&files), target, tmp.path()).unwrap();
        std::fs::remove_dir_all(tmp.path().join(target)).unwrap();
        // Nothing half-installed is left behind.
        let left: Vec<_> = std::fs::read_dir(tmp.path()).unwrap().collect();
        assert!(left.is_empty(), "{left:?}");
    }

    #[test]
    fn sha256_sums() {
        let data = b"pack";
        let hash: String = Sha256::digest(data)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let sums = format!("0000  other.tar.gz\n{hash}  velt-1-target-x.tar.gz\n");
        check_sha256(data, &sums, "velt-1-target-x.tar.gz").unwrap();
        assert!(check_sha256(b"tampered", &sums, "velt-1-target-x.tar.gz")
            .unwrap_err()
            .contains("SHA-256"));
        assert!(check_sha256(data, &sums, "missing.tar.gz")
            .unwrap_err()
            .contains("no entry"));
        // `sha256sum -b` writes `*<name>`.
        check_sha256(
            data,
            &format!("{hash} *velt-1-target-x.tar.gz\n"),
            "velt-1-target-x.tar.gz",
        )
        .unwrap();
    }

    #[test]
    fn only_release_targets_have_packs() {
        assert!(add("riscv64gc-unknown-linux-gnu", None)
            .unwrap_err()
            .contains("no target pack"));
        assert!(
            pack_name("x86_64-pc-windows-msvc").ends_with("-target-x86_64-pc-windows-msvc.tar.gz")
        );
    }
}
