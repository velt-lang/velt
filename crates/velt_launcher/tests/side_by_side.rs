//! The launcher against fake releases served from this machine (#948): packages pinned to
//! different versions run different toolchains, missing versions are downloaded and checked,
//! and `velt toolchain` manages them.

#[path = "support/releases.rs"]
mod releases;
mod support;

use std::process::Command;

use releases::*;
use support::fake_velt;

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
    std::fs::create_dir_all(checkout.join("std")).unwrap();
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

#[test]
fn a_plus_argument_picks_the_toolchain_for_one_command() {
    let releases = Releases::new(&["0.1.0", "0.2.0"]);
    let m = Machine::new(&releases);
    let pkg = m.package("app", Some("0.1"));
    let stdout = m.ok(&pkg, &["+0.2.0", "build", "--release"]);
    assert!(same(&prefix_of(&stdout), &m.toolchain("0.2.0")), "{stdout}");
    assert!(stdout.contains("selected=0.2.0 (+0.2.0)"), "{stdout}");
    // The `+` argument is the launcher's: the toolchain gets the rest.
    assert!(stdout.contains("args=build --release\n"), "{stdout}");
    // It wins over $VELT_TOOLCHAIN, and works for `velt toolchain` too.
    let stdout = m.ok_with(&pkg, &["+0.2.0", "run"], &[("VELT_TOOLCHAIN", "0.1.0")]);
    assert!(same(&prefix_of(&stdout), &m.toolchain("0.2.0")));
    let which = m.ok(&pkg, &["+0.2.0", "toolchain", "which"]);
    assert!(which.starts_with("0.2.0 (+0.2.0)"), "{which}");
    // A requirement, read like a pin: the newest installed 0.1.x.
    let stdout = m.ok(&pkg, &["+0.1", "build"]);
    assert!(same(&prefix_of(&stdout), &m.toolchain("0.1.0")), "{stdout}");
    assert!(stdout.contains("selected=0.1.0 (+0.1)"), "{stdout}");
    let stdout = m.ok_with(&pkg, &["build"], &[("VELT_TOOLCHAIN", "0.2")]);
    assert!(same(&prefix_of(&stdout), &m.toolchain("0.2.0")), "{stdout}");
    let err = m.fail(&pkg, &["+0.1.2.3", "build"], &[]);
    assert!(
        err.contains("`+0.1.2.3`: `0.1.2.3` is not a version"),
        "{err}"
    );
    let err = m.fail(&pkg, &["+nightly", "build"], &[]);
    assert!(err.contains("no toolchain is linked as `nightly`"), "{err}");
}

#[test]
fn an_index_names_a_newer_launcher_and_is_never_older_than_one_seen() {
    let releases = Releases::new(&["0.1.0"]);
    releases.index_with(&[("0.1.0", None)], Some("99.0.0"), Some(5_000));
    let m = Machine::new(&releases);
    let out = m.run(&m.work, &["toolchain", "list", "--available"], &[]);
    let (stdout, stderr) = text(&out);
    assert!(out.status.success(), "{stderr}");
    assert_eq!(stdout, "0.1.0\n");
    assert!(
        stderr.contains("a newer velt launcher (99.0.0) is available"),
        "{stderr}"
    );
    // A mirror serving an older (signed) index is refused.
    releases.index_with(&[("0.1.0", None)], None, Some(4_000));
    let err = m.fail(&m.work, &["toolchain", "list", "--available"], &[]);
    assert!(err.contains("is older than an index seen before"), "{err}");
}

#[test]
fn a_requirement_after_plus_installs_its_newest_release() {
    let releases = Releases::new(&["0.1.0", "0.2.0", "0.2.1"]);
    let m = Machine::new(&releases);
    let stdout = m.ok(&m.work, &["+0.2", "run", "x.vlt"]);
    assert!(same(&prefix_of(&stdout), &m.toolchain("0.2.1")), "{stdout}");
}

#[test]
fn a_launcher_never_runs_itself_or_others_without_end() {
    let releases = Releases::new(&["0.1.0"]);
    let m = Machine::new(&releases);
    // A link to the launcher's own root (written by hand: `link` refuses it).
    std::fs::create_dir_all(m.root.join("links")).unwrap();
    std::fs::write(m.root.join("links/loop"), format!("{}\n", m.root.display())).unwrap();
    let err = m.fail(&m.work, &["--version"], &[("VELT_TOOLCHAIN", "loop")]);
    assert!(err.contains("is this launcher"), "{err}");
    let err = m.fail(
        &m.work,
        &["toolchain", "link", "again", m.root.to_str().unwrap()],
        &[],
    );
    assert!(err.contains("is the root of a velt launcher"), "{err}");

    // Two roots whose links point at each other: each `bin/velt` is a launcher.
    let other = m.work.join("other");
    std::fs::create_dir_all(other.join("bin")).unwrap();
    std::fs::create_dir_all(other.join("std")).unwrap();
    std::fs::create_dir_all(other.join("links")).unwrap();
    std::fs::copy(
        env!("CARGO_BIN_EXE_velt-launcher"),
        other.join(format!("bin/velt{EXE}")),
    )
    .unwrap();
    std::fs::write(other.join("links/ping"), format!("{}\n", m.root.display())).unwrap();
    std::fs::write(m.root.join("links/ping"), format!("{}\n", other.display())).unwrap();
    let started = std::time::Instant::now();
    let err = m.fail(&m.work, &["--version"], &[("VELT_TOOLCHAIN", "ping")]);
    assert!(
        err.contains("velt launchers started one another 3 times"),
        "{err}"
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(30));
}

#[test]
fn downloads_only_over_https_or_from_this_machine() {
    let releases = Releases::new(&["0.1.0"]);
    let m = Machine::new(&releases);
    let pkg = m.package("app", Some("0.1"));
    for (base, what) in [
        (
            "http://releases.example",
            "plain http:// to another machine",
        ),
        ("file:///tmp/releases", "a file:// URL"),
    ] {
        let err = m.fail(&pkg, &["build"], &[("VELT_INSTALL_BASE_URL", base)]);
        assert!(
            err.contains(&format!("refusing to download {base}")) && err.contains(what),
            "{err}"
        );
    }
    assert_eq!(releases.downloads(), 0);
}

#[test]
fn the_launcher_says_its_own_version() {
    let releases = Releases::new(&["0.1.0"]);
    let m = Machine::new(&releases);
    let out = m.ok(&m.work, &["toolchain", "--version"]);
    assert_eq!(
        out,
        format!("velt-launcher {}\n", env!("CARGO_PKG_VERSION"))
    );
}

/// `velt toolchain prune` keeps the default and the newest of each minor, and removes the rest.
#[test]
fn prune_removes_old_patches_but_not_the_default() {
    let releases = Releases::new(&["0.1.0", "0.1.1", "0.1.2", "0.2.0"]);
    let m = Machine::new(&releases);
    for v in ["0.1.0", "0.1.1", "0.1.2", "0.2.0"] {
        m.ok(&m.work, &["toolchain", "install", v]);
    }
    // The first one installed is the default.
    let dry = m.ok(&m.work, &["toolchain", "prune", "--dry-run"]);
    assert!(
        dry.contains("would remove velt 0.1.1 (") && dry.contains("would free"),
        "{dry}"
    );
    assert!(
        dry.contains("nothing was removed") && !dry.contains("no longer run"),
        "{dry}"
    );
    assert!(
        !dry.contains("0.1.0") && !dry.contains("0.1.2") && !dry.contains("0.2.0"),
        "{dry}"
    );
    assert!(m.toolchain("0.1.1").exists());
    let out = m.ok(&m.work, &["toolchain", "prune"]);
    assert!(
        out.contains("removed velt 0.1.1 (") && out.contains("freed"),
        "{out}"
    );
    assert!(!m.toolchain("0.1.1").exists());
    for kept in ["0.1.0", "0.1.2", "0.2.0"] {
        assert!(m.toolchain(kept).exists(), "{kept}");
    }
    let again = m.ok(&m.work, &["toolchain", "prune"]);
    assert!(again.contains("nothing to prune"), "{again}");
}

/// `prune` keeps the version the current directory selects and versions a link points into.
#[test]
fn prune_keeps_what_this_directory_runs_and_linked_versions() {
    let releases = Releases::new(&["0.1.0", "0.1.1", "0.1.2", "0.2.0"]);
    let m = Machine::new(&releases);
    for v in ["0.1.0", "0.1.1", "0.1.2", "0.2.0"] {
        m.ok(&m.work, &["toolchain", "install", v]);
    }
    m.ok(&m.work, &["toolchain", "default", "0.2.0"]);
    let pinned = m.package("old", Some("=0.1.0"));
    let linked = m.toolchain("0.1.1");
    m.ok(
        &m.work,
        &["toolchain", "link", "dev", linked.to_str().unwrap()],
    );

    // In the package: 0.1.0 is what it runs, 0.1.1 is linked, 0.1.2 the newest of 0.1, 0.2.0
    // the default.
    let out = m.ok(&pinned, &["toolchain", "prune"]);
    assert!(out.contains("nothing to prune"), "{out}");
    // Outside it, 0.1.0 goes; the linked 0.1.1 stays, and its link still works.
    let out = m.ok(&m.work, &["toolchain", "prune"]);
    assert!(out.contains("removed velt 0.1.0 ("), "{out}");
    assert!(!out.contains("0.1.1"), "{out}");
    assert!(linked.exists());
    let stdout = m.ok(&m.work, &["+dev", "run"]);
    assert!(same(&prefix_of(&stdout), &linked), "{stdout}");
}

/// A newly installed patch version gets the target packs the version it follows had: the
/// launcher runs the new toolchain's `velt target add` for them.
#[test]
fn a_new_patch_version_gets_the_target_packs_of_the_one_it_follows() {
    let releases = Releases::new(&["0.1.0", "0.1.2", "0.2.0"]);
    let m = Machine::new(&releases);
    m.ok(&m.work, &["toolchain", "install", "0.1.0"]);
    // Packs `velt target add` installed into 0.1.0 (a staging directory, a stamp-less one and a
    // name that is no target are not packs).
    let targets = m.toolchain("0.1.0").join("lib/targets");
    for (pack, stamp) in [
        ("zz-test-target-a", true),
        ("zz-fail", true),
        ("zz-test-target-b", true),
        ("partial", false),
        ("--exit=3", true),
    ] {
        std::fs::create_dir_all(targets.join(pack)).unwrap();
        if stamp {
            std::fs::write(targets.join(pack).join("kit.stamp"), "").unwrap();
        }
    }
    let out = m.run(&m.work, &["toolchain", "install", "0.1.2"], &[]);
    let (stdout, stderr) = text(&out);
    assert!(out.status.success(), "{stdout}\n{stderr}");
    assert!(
        stderr.contains(
            "adding the target packs velt 0.1.0 had to velt 0.1.2: zz-fail, zz-test-target-a, \
             zz-test-target-b"
        ),
        "{stderr}"
    );
    // One `target add` per pack, the new toolchain's, its output on stderr: one that fails
    // doesn't stop the others, and only it is named.
    for pack in ["zz-test-target-a", "zz-test-target-b"] {
        assert!(
            stderr.contains(&format!("args=target add {pack}\n")),
            "{stderr}"
        );
    }
    assert!(same(&prefix_of(&stderr), &m.toolchain("0.1.2")), "{stderr}");
    assert!(
        stderr.contains("could not add zz-fail; run `velt +0.1.2 target add zz-fail`"),
        "{stderr}"
    );
    assert!(
        !stdout.contains("args=") && !stderr.contains("--exit=3"),
        "{stdout}{stderr}"
    );
    // Another minor starts without them.
    let (stdout, stderr) = text(&m.run(&m.work, &["toolchain", "install", "0.2.0"], &[]));
    assert!(
        !stdout.contains("target add") && !stderr.contains("adding"),
        "{stdout}{stderr}"
    );
}

/// In a pinned package, the toolchain `velt.lock.json` records runs (installed first when
/// missing), so every machine builds the commit with it; `velt update` runs the newest the pin
/// accepts, and a locked version the pin no longer accepts is not used.
#[test]
fn the_locked_toolchain_runs_in_its_package() {
    let releases = Releases::new(&["0.1.0", "0.1.2"]);
    let m = Machine::new(&releases);
    let pkg = m.package("app", Some("0.1"));
    let lock = |velt: &str| {
        std::fs::write(
            pkg.join("velt.lock.json"),
            format!("{{\n  \"version\": 1,\n  \"velt\": \"{velt}\",\n  \"packages\": []\n}}\n"),
        )
        .unwrap();
    };
    m.ok(&m.work, &["toolchain", "install", "0.1.2"]);

    // Locked to 0.1.0, which is not installed: installed and run, although 0.1.2 is.
    lock("0.1.0");
    let stdout = m.ok(&pkg, &["build"]);
    assert!(same(&prefix_of(&stdout), &m.toolchain("0.1.0")), "{stdout}");
    assert!(
        stdout.contains("selected=0.1.0 (velt.lock.json, with velt: \"0.1\" in "),
        "{stdout}"
    );
    let which = m.ok(&pkg, &["toolchain", "which"]);
    assert!(which.starts_with("0.1.0 (velt.lock.json"), "{which}");

    // `velt update` runs the newest the pin accepts (which then records itself).
    let stdout = m.ok(&pkg, &["update"]);
    assert!(same(&prefix_of(&stdout), &m.toolchain("0.1.2")), "{stdout}");
    assert!(
        stdout.contains("selected=0.1.2 (velt: \"0.1\" in "),
        "{stdout}"
    );

    // A locked version the pin doesn't accept is not used.
    lock("0.0.9");
    let stdout = m.ok(&pkg, &["build"]);
    assert!(same(&prefix_of(&stdout), &m.toolchain("0.1.2")), "{stdout}");
    // `+` and $VELT_TOOLCHAIN still win over the lock.
    lock("0.1.0");
    let stdout = m.ok(&pkg, &["+0.1.2", "build"]);
    assert!(same(&prefix_of(&stdout), &m.toolchain("0.1.2")), "{stdout}");
}

/// `velt toolchain install --from <file>` installs a downloaded archive without the network:
/// checked against the signed SHA256SUMS beside it, or unchecked with --unverified.
#[test]
fn install_from_a_downloaded_archive() {
    let releases = Releases::new(&[]);
    let m = Machine::new(&releases);
    let offline = [("VELT_INSTALL_BASE_URL", "http://127.0.0.1:9")];
    let dir = m.work.join("downloads");
    std::fs::create_dir_all(&dir).unwrap();
    let name = format!("velt-0.3.0-{}.tar.gz", host());
    let file = dir.join(&name);
    let bytes = archive("0.3.0");
    std::fs::write(&file, &bytes).unwrap();
    let sums = format!("{}  {name}\n", velt_toolchain::install::sha256_hex(&bytes));
    std::fs::write(dir.join("SHA256SUMS"), &sums).unwrap();
    std::fs::write(
        dir.join("SHA256SUMS.sig"),
        releases.signer.sign(sums.as_bytes()).as_ref(),
    )
    .unwrap();
    let path = file.to_str().unwrap();

    let out = m.ok_with(&m.work, &["toolchain", "install", "--from", path], &offline);
    assert!(
        out.contains("installed velt 0.3.0 from") && out.contains("is the default"),
        "{out}"
    );
    assert!(m.toolchain("0.3.0").join("std/VERSION").is_file());

    // Tampered: refused. No SHA256SUMS: refused unless --unverified.
    std::fs::write(&file, archive("0.3.9")).unwrap();
    m.ok(&m.work, &["toolchain", "remove", "0.3.0", "--force"]);
    let err = m.fail(&m.work, &["toolchain", "install", "--from", path], &offline);
    assert!(err.contains("SHA-256"), "{err}");
    // Signed, but not text: refused, not read lossily.
    std::fs::write(&file, &bytes).unwrap();
    let mut garbled = sums.clone().into_bytes();
    garbled.extend_from_slice(b"\xff\xfe\n");
    std::fs::write(dir.join("SHA256SUMS"), &garbled).unwrap();
    std::fs::write(
        dir.join("SHA256SUMS.sig"),
        releases.signer.sign(&garbled).as_ref(),
    )
    .unwrap();
    let err = m.fail(&m.work, &["toolchain", "install", "--from", path], &offline);
    assert!(err.contains("is not text"), "{err}");
    std::fs::remove_file(dir.join("SHA256SUMS")).unwrap();
    let err = m.fail(&m.work, &["toolchain", "install", "--from", path], &offline);
    assert!(err.contains("pass --unverified"), "{err}");
    // Unchecked, an archive still has to be what its name says (its top directory).
    std::fs::write(&file, &bytes).unwrap();
    let out = m.run(
        &m.work,
        &["toolchain", "install", "--from", path, "--unverified"],
        &offline,
    );
    let (stdout, stderr) = text(&out);
    assert!(out.status.success(), "{stdout}{stderr}");
    assert!(stderr.contains("without checking it"), "{stderr}");
    assert!(m.toolchain("0.3.0").join("std/VERSION").is_file());
    // Installed already: kept, and said so (not "installed … from").
    std::fs::write(&file, archive("0.3.9")).unwrap();
    let out = m.ok_with(
        &m.work,
        &["toolchain", "install", "--from", path, "--unverified"],
        &offline,
    );
    assert!(
        out.contains("velt 0.3.0 is already installed; kept it") && !out.contains("installed velt"),
        "{out}"
    );
    std::fs::write(&file, &bytes).unwrap();

    // Another machine's archive, or no file name to read the version from: refused.
    let other = dir.join("velt-0.3.0-some-other-triple.tar.gz");
    std::fs::write(&other, &bytes).unwrap();
    let err = m.fail(
        &m.work,
        &[
            "toolchain",
            "install",
            "--from",
            other.to_str().unwrap(),
            "--unverified",
        ],
        &offline,
    );
    assert!(
        err.contains("not a toolchain archive for this machine"),
        "{err}"
    );
    let err = m.fail(
        &m.work,
        &["toolchain", "install", "--unverified", "0.3.0"],
        &offline,
    );
    assert!(err.contains("--unverified goes with --from"), "{err}");
}
