use super::super::watch::SETTLE;
use super::*;
use std::time::{Duration, Instant};

/// The watcher reports no change for a while. It polls before each deadline check, so a
/// descheduled thread still polls once after a change would have settled.
fn stays_quiet(watcher: &mut Watcher) {
    let deadline = Instant::now() + SETTLE * 3;
    loop {
        assert!(watcher.poll().is_none(), "nothing changed");
        if Instant::now() > deadline {
            break;
        }
        std::thread::sleep(POLL);
    }
}

/// The watcher reports a change within a few seconds.
fn reports_a_change(watcher: &mut Watcher) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if watcher.poll().is_some() {
            return true;
        }
        if Instant::now() > deadline {
            return false;
        }
        std::thread::sleep(POLL);
    }
}

/// Every file under `dir`.
fn files_in(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            files_in(&path, out);
        } else {
            out.push(path);
        }
    }
}

/// A lockfile written just before `velt dev` started (`velt add`, `velt run`) was not saved
/// during the first build: no second build follows it (#733: that build reported
/// `hot-swapped 0 functions` after a later edit and left the old code running).
#[test]
fn quick_panics_restart_after_a_doubling_delay() {
    let secs = |quick| super::restart_delay(quick).as_secs();
    assert_eq!(
        [0, 1, 2, 3, 4, 5, 6, 7].map(secs),
        [0, 1, 2, 4, 8, 16, 30, 30]
    );
    assert_eq!(secs(u32::MAX), 30, "no overflow however many");
}

#[test]
fn a_recent_lockfile_needs_no_second_build() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join(vpm::manifest::MANIFEST_FILE),
        "export const pkg: Package = { name: \"app\", version: \"0.1.0\" };\n",
    )
    .unwrap();
    let main = root.join("main.velt");
    std::fs::write(&main, "function main() {}\n").unwrap();
    std::fs::write(root.join(vpm::lockfile::LOCK_FILE), "{}\n").unwrap();
    let input = Some(main.as_path());
    let mut watcher = first_watcher(Watcher::new(false), input, None);
    let snapshot = watcher.snapshot();
    // What the first build read.
    let read = std::iter::once(main.clone()).chain(manifest_files(input));
    watcher.set(read, &snapshot);
    stays_quiet(&mut watcher);
}

/// `velt add util` and right away `velt dev`: the package fetched into the cache moments
/// before the first build was not saved during it, so no second build follows. An edit to a
/// cached file after the first build still counts.
#[test]
fn a_package_fetched_just_before_needs_no_second_build() {
    let dir = tempfile::tempdir().unwrap();
    let loc = vpm::Locations::under(&dir.path().join("home"));
    let util = vpm::scaffold::new_package(dir.path(), "util", true).unwrap();
    vpm::registry::publish_local(&util, &loc).unwrap();
    let app = vpm::scaffold::new_package(dir.path(), "app", false).unwrap();
    let spec = vpm::edit::DependencySpec {
        version: Some("0.1.0".into()),
        path: None,
        ..Default::default()
    };
    // `velt add util`: the manifest, the lockfile and the package in the cache.
    vpm::edit::add_dependency(&app, "util", &spec).unwrap();
    vpm::install(&app, &loc, vpm::InstallOptions::default()).unwrap();
    let cached = loc.cache.join("util-0.1.0");
    let mut fetched = vec![];
    files_in(&cached, &mut fetched);
    assert!(!fetched.is_empty(), "nothing in {}", cached.display());

    // `velt dev`: the first build reads the program, the package and the manifest.
    let main = app.join("src").join("main.velt");
    let input = Some(main.as_path());
    let first_build = |watcher: &mut Watcher| {
        let snapshot = watcher.snapshot();
        let read = std::iter::once(main.clone())
            .chain(fetched.iter().cloned())
            .chain(manifest_files(input));
        watcher.set(read, &snapshot);
    };
    let mut watcher = first_watcher(Watcher::new(false), input, Some(&loc.cache));
    first_build(&mut watcher);
    stays_quiet(&mut watcher);

    // An edit there afterwards (another length, so any clock sees it) is a change.
    let lib = fetched
        .iter()
        .find(|f| vpm::sources::is_source_file(f))
        .unwrap();
    std::fs::write(
        lib,
        "export function changed() {}\n// edited in the cache\n",
    )
    .unwrap();
    assert!(
        reports_a_change(&mut watcher),
        "edited after the first build"
    );

    // Without the cache known as written once, a second build follows (the files are
    // recent and the snapshot doesn't cover them): what this test guards against.
    let mut watcher = first_watcher(Watcher::new(false), input, None);
    first_build(&mut watcher);
    assert!(
        reports_a_change(&mut watcher),
        "recent files count as saved"
    );
}

/// A program with a path dependency and no lockfile yet: the install writes the lockfile.
/// Done before the watcher is seeded (`settle_lockfile`), the first build sees it as it
/// was, and no second build follows; written by the first build itself, it looks saved
/// during it, and the program is built (and restarted) again right after starting.
#[test]
fn a_lockfile_the_install_writes_needs_no_second_build() {
    let dir = tempfile::tempdir().unwrap();
    let loc = vpm::Locations::under(&dir.path().join("home"));
    vpm::scaffold::new_package(dir.path(), "util", true).unwrap();
    let app = vpm::scaffold::new_package(dir.path(), "app", false).unwrap();
    let spec = vpm::edit::DependencySpec {
        version: None,
        path: Some("../util".into()),
        ..Default::default()
    };
    vpm::edit::add_dependency(&app, "util", &spec).unwrap();
    let lock = app.join(vpm::lockfile::LOCK_FILE);
    let _ = std::fs::remove_file(&lock);
    let main = app.join("src").join("main.velt");
    let input = Some(main.as_path());
    let install = || {
        vpm::install(&app, &loc, vpm::InstallOptions::default()).unwrap();
    };
    let first_build = |watcher: &mut Watcher, installs: bool| {
        let snapshot = watcher.snapshot();
        if installs {
            install();
        }
        let read = std::iter::once(main.clone()).chain(manifest_files(input));
        watcher.set(read, &snapshot);
    };

    // The fix: installed first, then watched.
    install();
    assert!(lock.is_file());
    let mut watcher = first_watcher(Watcher::new(false), input, None);
    first_build(&mut watcher, true);
    stays_quiet(&mut watcher);

    // Without it: the first build writes the lockfile, which counts as a change.
    std::fs::remove_file(&lock).unwrap();
    let mut watcher = first_watcher(Watcher::new(false), input, None);
    first_build(&mut watcher, true);
    assert!(
        reports_a_change(&mut watcher),
        "the lockfile the build wrote"
    );
}
