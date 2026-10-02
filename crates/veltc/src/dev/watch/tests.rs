use super::*;

/// Poll until a change is reported (or `limit` passes).
fn wait(w: &mut Watcher, limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if w.poll().is_some() {
            return true;
        }
        std::thread::sleep(POLL);
    }
    false
}

/// Edits, files that appear, saves during the build and new `.vlt` files next to watched
/// ones, each reported once after it settles.
fn reports_changes(notify: bool) {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("main.vlt");
    std::fs::write(&file, "a").unwrap();
    let mut w = Watcher::new(notify);
    assert_eq!(w.notifies(), notify);
    let snap = w.snapshot();
    w.set([file.clone(), dir.path().join("missing.vlt")], &snap);
    assert!(!wait(&mut w, SETTLE * 3), "nothing changed");
    std::fs::write(&file, "bb").unwrap();
    assert!(w.poll().is_none(), "not settled yet");
    assert!(wait(&mut w, Duration::from_secs(5)));
    assert!(!wait(&mut w, SETTLE * 3), "reported once");
    // A file that appears counts as a change.
    std::fs::write(dir.path().join("missing.vlt"), "x").unwrap();
    assert!(wait(&mut w, Duration::from_secs(5)));
    // So does a new module nothing has read yet; other new files don't.
    std::fs::write(dir.path().join("notes.txt"), "x").unwrap();
    assert!(!wait(&mut w, SETTLE * 5), "not a source file");
    let added = dir.path().join("added.vlt");
    std::fs::write(&added, "x").unwrap();
    assert!(wait(&mut w, Duration::from_secs(5)));
    // Deleted and created again (`git stash`, a branch switch): new again.
    std::fs::remove_file(&added).unwrap();
    assert!(!wait(&mut w, SETTLE * 3), "a deletion alone needs no build");
    std::fs::write(&added, "x").unwrap();
    assert!(wait(&mut w, Duration::from_secs(5)), "recreated");
    // An editor's atomic save: write a temporary file, rename it over the original.
    let tmp = dir.path().join("main.vlt.tmp");
    std::fs::write(&tmp, "atomic save").unwrap();
    std::fs::rename(&tmp, &file).unwrap();
    assert!(wait(&mut w, Duration::from_secs(5)), "renamed over");
    assert!(!wait(&mut w, SETTLE * 3), "reported once");
    saves_during_builds(&mut w, dir.path());
}

/// A build starts (snapshot), reads its files, and the watcher learns what it read only when it
/// ends: every save in between must lead to another build, none may be lost.
fn saves_during_builds(w: &mut Watcher, dir: &Path) {
    let file = dir.join("main.vlt");
    let snap = w.snapshot();
    let read = [file.clone(), dir.join("added.vlt"), dir.join("missing.vlt")];
    w.set(read.clone(), &snap);
    assert!(!wait(w, SETTLE * 3), "a build during which nothing changed");
    // A watched file saved again while the build that read it was running.
    let snap = w.snapshot();
    std::fs::write(&file, "saved during the build").unwrap();
    w.set(read.clone(), &snap);
    assert!(wait(w, Duration::from_secs(5)), "saved during a build");
    // A new module the failed build read half-written: its last write came during the build,
    // after the build read it and before the watcher knew the file (#271).
    let late = dir.join("late.vlt");
    let snap = w.snapshot();
    std::fs::write(&late, "").unwrap();
    std::fs::write(&late, "export function late() {}").unwrap();
    w.add([late.clone()], &snap);
    assert!(
        wait(w, Duration::from_secs(5)),
        "finished during a failed build"
    );
    assert!(!wait(w, SETTLE * 3), "reported once");
    // The same with the module already there, empty, when the build started: its creation
    // started the build, which read it before its content was written.
    let half = dir.join("half.vlt");
    std::fs::write(&half, "").unwrap();
    assert!(wait(w, Duration::from_secs(5)), "a new module");
    let snap = w.snapshot();
    std::fs::write(&half, "export function half() {}").unwrap();
    w.add([half.clone()], &snap);
    assert!(
        wait(w, Duration::from_secs(5)),
        "completed during a failed build"
    );
    // A module that appeared during a build that didn't read it (the import still failed).
    let snap = w.snapshot();
    std::fs::write(dir.join("other.vlt"), "x").unwrap();
    w.add([file.clone()], &snap);
    assert!(
        wait(w, Duration::from_secs(5)),
        "appeared during a failed build"
    );
}

#[test]
fn reports_changes_by_polling() {
    reports_changes(false);
}

#[test]
fn reports_changes_from_notifications() {
    if Watcher::new(true).notifies() {
        reports_changes(true);
    }
}
