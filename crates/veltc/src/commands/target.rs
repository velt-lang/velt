//! `velt target list|add|remove`: target packs, what `velt build --target <triple>` needs to
//! build for a platform other than this one (#856). A pack is `velt-<version>-target-<triple>.tar.gz`,
//! a release asset holding `<triple>/`: the target's runtime library and its link kit
//! (`velt_link::kit`). It is installed into `<prefix>/lib/targets/<triple>/`, where `velt_link`
//! looks for both.

use std::path::{Path, PathBuf};

use velt_toolchain::install::{check_sha256, install_dir, sha256_entry, Existing};

use crate::cli::target::TargetAction;

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
            let release = format!(
                "{}/releases/download/v{}",
                velt_toolchain::release::base_url(),
                env!("CARGO_PKG_VERSION")
            );
            // The expected hash first, so a pack the release lacks fails before a long download.
            let sums = match &pinned {
                Some(sums) => sums.clone(),
                None => {
                    // A toolchain without pack hashes (built from source): the release's own
                    // `SHA256SUMS`, which its signature shows the velt project published.
                    let key = velt_toolchain::signature::public_key()?;
                    let sums = velt_toolchain::signature::signed_sums(&release, &key)?.ok_or_else(
                        || {
                            format!(
                                "{release}/SHA256SUMS does not exist (HTTP 404): this velt's \
                                 release ({}) has no target packs; a toolchain built from source \
                                 installs packs with `--from`",
                                env!("CARGO_PKG_VERSION")
                            )
                        },
                    )?;
                    sha256_entry(&sums, &name)
                        .ok_or_else(|| format!("the release's SHA256SUMS has no {name}"))?;
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

/// GET `url` ([`velt_toolchain::install::download`]); a missing file is a pack this velt's
/// release does not have.
fn download(url: &str) -> Result<Vec<u8>, String> {
    velt_toolchain::install::download(url)?.ok_or_else(|| {
        format!(
            "{url} does not exist (HTTP 404): this velt's release ({}) has no such \
             target pack; a toolchain built from source installs packs with `--from`",
            env!("CARGO_PKG_VERSION")
        )
    })
}

/// Unpack a pack (a `.tar.gz` of `<target>/...`) into `<dir>/<target>`, replacing an installed
/// one only once the new one is complete.
fn install(archive: &[u8], target: &str, dir: &Path) -> Result<(), String> {
    install_dir(
        archive,
        target,
        &dir.join(target),
        "target pack",
        Existing::Replace,
        |staging| pack_problem(staging, target),
    )
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
    fn only_release_targets_have_packs() {
        assert!(add("riscv64gc-unknown-linux-gnu", None, false)
            .unwrap_err()
            .contains("no target pack"));
        assert!(
            pack_name("x86_64-pc-windows-msvc").ends_with("-target-x86_64-pc-windows-msvc.tar.gz")
        );
    }
}
