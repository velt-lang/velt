//! `velt toolchain self-update` against fake releases served from this machine.

#[path = "support/releases.rs"]
mod releases;
mod support;

use std::process::Command;

use releases::*;
use support::fake_velt;

/// The names in `<root>/bin` starting with `.` (what a self-update stages or moves aside).
fn hidden_in_bin(m: &Machine) -> Vec<String> {
    std::fs::read_dir(m.root.join("bin"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with('.'))
        .collect()
}

/// `velt toolchain self-update` replaces the launcher with the one of the newest release the
/// index names (installing that toolchain first), and says when there is none newer. A release
/// launcher that doesn't run as that version is refused, and `bin/velt` kept.
#[test]
fn self_update_replaces_the_launcher_with_the_newest_one() {
    let releases = Releases::new(&["99.0.0"]);
    let own = env!("CARGO_PKG_VERSION");
    releases.index_with(&[("99.0.0", None)], Some(own), None);
    let m = Machine::new(&releases);
    let out = m.ok(&m.work, &["toolchain", "self-update"]);
    assert!(
        out.contains(&format!("the velt launcher is up to date ({own})")),
        "{out}"
    );

    releases.index_with(&[("99.0.0", None)], Some("99.0.0"), None);
    let launcher = m.root.join(format!("bin/velt{EXE}"));
    let before = std::fs::read(&launcher).unwrap();
    // The release's launcher must run here and say it is 99.0.0: one that doesn't answer as a
    // launcher, or names another version, is never put in place.
    for env in [&[][..], &[("FAKE_VELT_LAUNCHER_VERSION", "98.0.0")][..]] {
        let stderr = m.fail(&m.work, &["toolchain", "self-update"], env);
        assert!(
            stderr.contains("the launcher of velt 99.0.0")
                && stderr.contains("does not work here")
                && stderr.contains("not `velt-launcher 99.0.0`"),
            "{stderr}"
        );
        assert_eq!(std::fs::read(&launcher).unwrap(), before);
        assert!(hidden_in_bin(&m).is_empty(), "{:?}", hidden_in_bin(&m));
    }

    // A launcher moved aside by an earlier self-update on Windows is swept away.
    let aside = m.root.join(format!("bin/.velt{EXE}.old-1"));
    std::fs::write(&aside, b"old").unwrap();
    let env = [("FAKE_VELT_LAUNCHER_VERSION", "99.0.0")];
    let out = m.run(&m.work, &["toolchain", "self-update"], &env);
    let (stdout, stderr) = text(&out);
    assert!(out.status.success(), "{stdout}{stderr}");
    assert!(
        stdout.contains(&format!("updated the velt launcher: {own} → 99.0.0")),
        "{stdout}"
    );
    assert!(!aside.exists(), "the moved-aside launcher is swept");
    // bin/velt is now that release's launcher (here the stand-in toolchain).
    assert_eq!(
        std::fs::read(&launcher).unwrap(),
        std::fs::read(fake_velt()).unwrap()
    );
    assert!(hidden_in_bin(&m).is_empty(), "{:?}", hidden_in_bin(&m));
}

/// The first self-update installs the release the launcher comes from.
#[test]
fn self_update_installs_the_newest_release_first() {
    let releases = Releases::new(&["99.0.0"]);
    releases.index_with(&[("99.0.0", None)], Some("99.0.0"), None);
    let m = Machine::new(&releases);
    let env = [("FAKE_VELT_LAUNCHER_VERSION", "99.0.0")];
    let out = m.run(&m.work, &["toolchain", "self-update"], &env);
    let (stdout, stderr) = text(&out);
    assert!(out.status.success(), "{stdout}{stderr}");
    assert!(
        stderr.contains("installing velt 99.0.0, which brings the launcher"),
        "{stderr}"
    );
    assert!(m.toolchain("99.0.0").is_dir());
}

/// Only `<root>/bin/velt` updates itself: a launcher copied elsewhere in `bin` would overwrite a
/// `velt` it isn't.
#[test]
fn self_update_runs_only_as_bin_velt() {
    let releases = Releases::new(&["99.0.0"]);
    releases.index_with(&[("99.0.0", None)], Some("99.0.0"), None);
    let m = Machine::new(&releases);
    let other = m.root.join(format!("bin/velt-other{EXE}"));
    std::fs::copy(env!("CARGO_BIN_EXE_velt-launcher"), &other).unwrap();
    let launcher = m.root.join(format!("bin/velt{EXE}"));
    std::fs::write(&launcher, b"the user's own").unwrap();
    let out = Command::new(&other)
        .args(["toolchain", "self-update"])
        .current_dir(&m.work)
        .env("VELT_INSTALL_BASE_URL", &m.base)
        .env("VELT_INSTALL_PUBLIC_KEY", &m.key)
        .env("FAKE_VELT_LAUNCHER_VERSION", "99.0.0")
        .output()
        .unwrap();
    let (stdout, stderr) = text(&out);
    assert!(!out.status.success(), "{stdout}");
    assert!(
        stderr.contains("is not") && stderr.contains("so it can't update it"),
        "{stderr}"
    );
    assert_eq!(std::fs::read(&launcher).unwrap(), b"the user's own");
}

/// An index signed by a key this root has revoked (#1009, `<root>/revoked-keys`) is refused:
/// self-update installs nothing and keeps `bin/velt`.
#[test]
fn self_update_refuses_an_index_signed_by_a_revoked_key() {
    let releases = Releases::new(&["99.0.0"]);
    releases.index_with(&[("99.0.0", None)], Some("99.0.0"), None);
    let m = Machine::new(&releases);
    // Trusted: the releases' key and a backup; the releases' key is revoked here.
    let backup = "22".repeat(32);
    let keys = format!("{},{backup}", m.key);
    std::fs::write(m.root.join("revoked-keys"), format!("{}\n", m.key)).unwrap();
    let launcher = m.root.join(format!("bin/velt{EXE}"));
    let before = std::fs::read(&launcher).unwrap();
    let env = [
        ("VELT_INSTALL_PUBLIC_KEY", keys.as_str()),
        ("FAKE_VELT_LAUNCHER_VERSION", "99.0.0"),
    ];
    let stderr = m.fail(&m.work, &["toolchain", "self-update"], &env);
    assert!(
        stderr.contains("releases.json") && stderr.contains("does not match the release key"),
        "{stderr}"
    );
    assert_eq!(std::fs::read(&launcher).unwrap(), before);
    assert!(!m.toolchain("99.0.0").exists());
    assert_eq!(releases.downloads(), 0);
}

/// A launcher a self-update staged but never put in place (`.velt-new-*`) is swept once stale;
/// a fresh one may be a running self-update's, and is kept.
#[test]
fn stale_staged_launchers_are_swept() {
    let releases = Releases::new(&["0.1.0"]);
    let m = Machine::new(&releases);
    let stale = m.root.join(format!("bin/.velt-new-1{EXE}"));
    let fresh = m.root.join(format!("bin/.velt-new-2{EXE}"));
    std::fs::write(&stale, b"half").unwrap();
    std::fs::write(&fresh, b"new").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&stale)
        .unwrap()
        .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(7200))
        .unwrap();
    m.ok(&m.work, &["toolchain", "--version"]);
    assert!(!stale.exists());
    assert!(fresh.exists());
}
