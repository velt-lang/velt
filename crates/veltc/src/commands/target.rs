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
        TargetAction::Add {
            targets,
            from,
            unverified,
        } => {
            for target in targets {
                add(target, from.as_deref(), *unverified)?;
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
                .map(|e| e.file_name().to_string_lossy().into_owned())
                // `.<triple>.<pid>` directories are an `add` in progress (or one that died).
                .filter(|name| !name.starts_with('.'))
                .filter(|name| dir.join(name).join(velt_link::kit::STAMP).is_file())
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

/// The pack hashes a release toolchain carries (`lib/targets/PACKS.sha256`, written by the
/// release workflow once every pack is built): they come with the toolchain the user installed
/// and checked, so they also tell a substituted pack from the real one, which the release's own
/// `SHA256SUMS` (fetched beside the pack) cannot.
const PACK_HASHES: &str = "PACKS.sha256";

fn known_targets() -> String {
    velt_link::kit::RELEASE_TARGETS.join(", ")
}

fn add(target: &str, from: Option<&Path>, unverified: bool) -> Result<(), String> {
    if !velt_link::kit::RELEASE_TARGETS.contains(&target) {
        return Err(format!(
            "no target pack exists for `{target}`; the targets are: {}",
            known_targets()
        ));
    }
    if velt_link::same_target(target, &velt_link::host_triple()) {
        println!("{target} is this machine: the toolchain builds for it already");
        return Ok(());
    }
    let dir = targets_dir()?;
    let name = pack_name(target);
    let pinned = std::fs::read_to_string(dir.join(PACK_HASHES))
        .ok()
        .filter(|sums| sha256_entry(sums, &name).is_some());
    let archive = match from {
        Some(file) => {
            let bytes =
                std::fs::read(file).map_err(|e| format!("cannot read {}: {e}", file.display()))?;
            if let Some(sums) = &pinned {
                check_sha256(&bytes, sums, &name).map_err(|e| {
                    format!(
                        "{e} ({} lists the pack of this velt's release)",
                        PACK_HASHES
                    )
                })?;
            } else if let Ok(sums) = std::fs::read_to_string(file.with_file_name("SHA256SUMS")) {
                let file_name = file.file_name().unwrap_or_default().to_string_lossy();
                check_sha256(&bytes, &sums, &file_name)?;
                eprintln!(
                    "note: {} matches the SHA256SUMS beside it, which shows it is intact, not \
                     where it comes from (this toolchain lists no hash for it)",
                    file.display()
                );
            } else if unverified {
                eprintln!(
                    "warning: installing {} without verifying it",
                    file.display()
                );
            } else {
                return Err(format!(
                    "cannot verify {}: this toolchain lists no hash for {name} and there is no \
                     SHA256SUMS beside it; pass `--unverified` to install it anyway (a pack you \
                     built yourself)",
                    file.display()
                ));
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
            // The expected hash first, so a pack the release lacks fails before a long download.
            let sums = match &pinned {
                Some(sums) => sums.clone(),
                None => {
                    let sums = download(&format!("{release}/SHA256SUMS"))?;
                    let sums = String::from_utf8_lossy(&sums).into_owned();
                    sha256_entry(&sums, &name)
                        .ok_or_else(|| format!("the release's SHA256SUMS has no {name}"))?;
                    eprintln!(
                        "note: this toolchain lists no pack hashes ({PACK_HASHES}); checking \
                         against the release's SHA256SUMS, which shows the pack is intact, not \
                         where it comes from"
                    );
                    sums
                }
            };
            eprintln!("downloading {name}...");
            let bytes = download(&format!("{release}/{name}"))?;
            check_sha256(&bytes, &sums, &name)?;
            bytes
        }
    };
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

/// The hash for `name` in a `SHA256SUMS`-style file (`<hex>  <name>`, or `*<name>`).
fn sha256_entry(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (hash, file) = line.split_once(char::is_whitespace)?;
        (file.trim().trim_start_matches('*') == name).then(|| hash.to_ascii_lowercase())
    })
}

/// Check `bytes` against the line for `name` in a `SHA256SUMS`-style file.
fn check_sha256(bytes: &[u8], sums: &str, name: &str) -> Result<(), String> {
    let expected =
        sha256_entry(sums, name).ok_or_else(|| format!("SHA256SUMS has no entry for {name}"))?;
    let actual: String = Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if actual != expected {
        return Err(format!(
            "{name} has SHA-256 {actual}, but the expected one is {expected}"
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
            Some(why) => Err(format!(
                "the target pack for {target} cannot be used: {why}"
            )),
            None => Ok(()),
        });
    if let Err(e) = result {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(e);
    }
    swap_into_place(&staging, &dir.join(target))
}

/// Move the complete pack `staging` to `dest`: an installed pack is renamed aside first and
/// deleted only once the new one is in place, and put back when the move fails, so `dest` is
/// never half deleted (a file of the old pack still open, as Windows forbids deleting, only
/// leaves the renamed copy behind).
fn swap_into_place(staging: &Path, dest: &Path) -> Result<(), String> {
    let name = dest.file_name().unwrap_or_default().to_string_lossy();
    let old = dest.with_file_name(format!(".{name}.old.{}", std::process::id()));
    let had_old = dest.exists();
    if had_old {
        if let Err(e) = std::fs::rename(dest, &old) {
            let _ = std::fs::remove_dir_all(staging);
            return Err(format!("cannot replace {} (in use?): {e}", dest.display()));
        }
    }
    if let Err(e) = std::fs::rename(staging, dest) {
        if had_old {
            let _ = std::fs::rename(&old, dest);
        }
        let _ = std::fs::remove_dir_all(staging);
        return Err(format!("cannot install into {}: {e}", dest.display()));
    }
    if had_old {
        let _ = std::fs::remove_dir_all(&old);
    }
    Ok(())
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
    remove_from(target, &targets_dir()?)
}

/// [`remove`] in the targets directory `dir`. Only a release target's name is accepted: the
/// argument is joined onto `dir`, so anything else (`..`, an absolute path) could name another
/// directory.
fn remove_from(target: &str, dir: &Path) -> Result<(), String> {
    if !velt_link::kit::RELEASE_TARGETS.contains(&target) {
        return Err(format!(
            "`{target}` is not a target; the targets are: {}",
            known_targets()
        ));
    }
    if velt_link::same_target(target, &velt_link::host_triple()) {
        return Err(format!(
            "`{target}` is this machine: it is part of the toolchain"
        ));
    }
    let dir = dir.join(target);
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
            err.contains("cannot be used") && err.contains("libvelt_rt.a"),
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
            .contains("expected"));
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
    fn remove_takes_target_names_only() {
        let tmp = tempfile::tempdir().unwrap();
        let targets = tmp.path().join("lib/targets");
        std::fs::create_dir_all(&targets).unwrap();
        std::fs::write(tmp.path().join("lib/keep"), b"").unwrap();
        let outside = tempfile::tempdir().unwrap();
        for bad in [
            "..",
            "../..",
            ".",
            "",
            outside.path().to_str().unwrap(),
            "x/../..",
        ] {
            let err = remove_from(bad, &targets).unwrap_err();
            assert!(err.contains("is not a target"), "{bad}: {err}");
        }
        assert!(tmp.path().join("lib/keep").exists() && outside.path().exists());
        let t = "x86_64-unknown-linux-musl";
        std::fs::create_dir_all(targets.join(t)).unwrap();
        remove_from(t, &targets).unwrap();
        assert!(!targets.join(t).exists());
        assert!(remove_from(t, &targets)
            .unwrap_err()
            .contains("not installed"));
    }

    #[test]
    fn a_replaced_pack_is_swapped_whole() {
        let tmp = tempfile::tempdir().unwrap();
        let (staging, dest) = (tmp.path().join(".t.1"), tmp.path().join("t"));
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("old"), b"").unwrap();
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(staging.join("new"), b"").unwrap();
        swap_into_place(&staging, &dest).unwrap();
        assert!(dest.join("new").exists() && !dest.join("old").exists());
        let left: Vec<_> = std::fs::read_dir(tmp.path()).unwrap().collect();
        assert_eq!(left.len(), 1, "{left:?}");
        // A failed move puts the old pack back.
        let missing = tmp.path().join(".gone");
        assert!(swap_into_place(&missing, &dest).is_err());
        assert!(dest.join("new").exists());
    }

    #[test]
    fn only_release_targets_have_packs() {
        assert!(add("riscv64gc-unknown-linux-gnu", None, false)
            .unwrap_err()
            .contains("no target pack"));
        assert!(
            pack_name("x86_64-pc-windows-msvc").ends_with("-target-x86_64-pc-windows-msvc.tar.gz")
        );
    }
}
