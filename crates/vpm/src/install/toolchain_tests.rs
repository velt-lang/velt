use super::*;

fn opts(locked: bool, update: bool) -> InstallOptions {
    InstallOptions {
        locked,
        update,
        target: None,
        ..Default::default()
    }
}

#[test]
fn the_lock_keeps_its_toolchain_until_update_or_the_pin_moves() {
    let normal = opts(false, false);
    // A new lock records the running toolchain.
    assert_eq!(
        locked_toolchain(None, Some("0.2"), "0.2.1", &normal).as_deref(),
        Some("0.2.1")
    );
    // Without a pin, nothing is recorded (and a recorded one is dropped).
    assert_eq!(locked_toolchain(None, None, "0.2.1", &normal), None);
    assert_eq!(
        locked_toolchain(Some("0.1.0"), None, "0.2.1", &normal),
        None
    );
    // Another toolchain running the package (an editor, $VELT_TOOLCHAIN) leaves it alone.
    assert_eq!(
        locked_toolchain(Some("0.2.0"), Some("0.2"), "0.2.1", &normal).as_deref(),
        Some("0.2.0")
    );
    // `velt update` moves it to the running toolchain (the launcher ran the newest).
    assert_eq!(
        locked_toolchain(Some("0.2.0"), Some("0.2"), "0.2.1", &opts(false, true)).as_deref(),
        Some("0.2.1")
    );
    // A pin that no longer accepts it moves it too.
    assert_eq!(
        locked_toolchain(Some("0.2.0"), Some("0.3"), "0.3.0", &normal).as_deref(),
        Some("0.3.0")
    );
    // --locked never adds or moves it.
    let locked = opts(true, false);
    assert_eq!(locked_toolchain(None, Some("0.2"), "0.2.1", &locked), None);
    assert_eq!(
        locked_toolchain(Some("0.2.0"), Some("0.3"), "0.3.0", &locked).as_deref(),
        Some("0.2.0")
    );
}

#[test]
fn locked_checks_the_toolchain_in_pinned_packages_only() {
    check_locked_toolchain(Some("0.2.0"), Some("0.2"), "0.2.0").unwrap();
    check_locked_toolchain(None, Some("0.2"), "0.2.0").unwrap();
    // Unpinned: the launcher doesn't follow the lock there, so it isn't checked.
    check_locked_toolchain(Some("0.1.0"), None, "0.2.0").unwrap();
    let other = check_locked_toolchain(Some("0.2.0"), Some("0.2"), "0.2.1").unwrap_err();
    assert!(
        other.contains("runs 0.2.0 here (`velt +0.2.0 …`)"),
        "{other}"
    );
    let moved = check_locked_toolchain(Some("0.1.9"), Some("0.2"), "0.2.0").unwrap_err();
    assert!(
        moved.contains("pins velt 0.1.9, which the package's velt pin \"0.2\" no longer accepts")
            && moved.contains("to record velt 0.2.0"),
        "{moved}"
    );
}
