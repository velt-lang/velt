//! `velt cache warm`: what it stores for debug and release builds, and its marker.

use super::*;

/// `velt cache warm` is what stores objects for debug builds; a build of the same program then
/// compiles none of them.
#[test]
fn the_warm_up_stores_what_debug_builds_link() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    let cache = dir.join("cache");
    let share = dir.join("share");
    std::fs::create_dir_all(share.join("warm")).unwrap();
    std::fs::write(share.join("warm/a.velt"), PROGRAM).unwrap();
    std::fs::write(dir.join("a.velt"), PROGRAM).unwrap();
    runtime_support::build_native_runtime(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
    let mut cmd = crate::no_window::command(env!("CARGO_BIN_EXE_velt"));
    let warm = (cmd.args(["cache", "warm"]))
        .current_dir(dir)
        .env("VELT_BUILD_CACHE", &cache)
        .env("VELT_SHARE", &share)
        .output()
        .unwrap();
    assert!(warm.status.success(), "{}", text(&warm.stderr));
    assert!(!entries(&cache).is_empty(), "{}", text(&warm.stderr));
    let built = debug_build(dir, "a.velt", "a", &cache, &[]);
    let (found, compiled) = counts(&built);
    assert!(found > 0 && compiled == 0, "{}", text(&built.stderr));
    assert_eq!(run(dir, "a"), OUTPUT);
}

/// A warm-up program that fails doesn't make the next warm-up build every program again: the
/// marker is written with what failed, and the next run returns at once.
#[test]
fn a_failed_warm_up_program_is_not_warmed_again() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    let cache = dir.join("cache");
    let share = dir.join("share");
    std::fs::create_dir_all(share.join("warm")).unwrap();
    std::fs::write(share.join("warm/a.velt"), PROGRAM).unwrap();
    std::fs::write(share.join("warm/b.velt"), "function main() { let }\n").unwrap();
    runtime_support::build_native_runtime(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
    let warm = || {
        let mut cmd = crate::no_window::command(env!("CARGO_BIN_EXE_velt"));
        (cmd.args(["cache", "warm"]))
            .current_dir(dir)
            .env("VELT_BUILD_CACHE", &cache)
            .env("VELT_SHARE", &share)
            .output()
            .unwrap()
    };
    let first = warm();
    let err = text(&first.stderr);
    assert!(!first.status.success(), "{err}");
    assert!(err.contains("b.velt"), "{err}");
    assert!(!entries(&cache).is_empty(), "a.velt warmed: {err}");
    let second = warm();
    let err = text(&second.stderr);
    assert!(second.status.success(), "{err}");
    assert!(err.contains("warm for this velt already"), "{err}");
}

/// `velt cache warm` stores what release builds link too: a release build of a warm-up program
/// then finds its `@noInline` functions and leaves none in the program.
#[test]
fn the_warm_up_stores_what_release_builds_link() {
    let tmp = test_dir::TestDir::new();
    let dir = tmp.path();
    let cache = dir.join("cache");
    let share = dir.join("share");
    std::fs::create_dir_all(share.join("warm")).unwrap();
    std::fs::write(share.join("warm/a.velt"), NO_INLINE_PROGRAM).unwrap();
    std::fs::write(dir.join("a.velt"), NO_INLINE_PROGRAM).unwrap();
    runtime_support::build_native_runtime(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
    let mut cmd = crate::no_window::command(env!("CARGO_BIN_EXE_velt"));
    let warm = (cmd.args(["cache", "warm"]))
        .current_dir(dir)
        .env("VELT_BUILD_CACHE", &cache)
        .env("VELT_SHARE", &share)
        .env_remove("VELT_STD_OBJECT")
        .output()
        .unwrap();
    assert!(warm.status.success(), "{}", text(&warm.stderr));
    let built = release_default(dir, "a.velt", "a", &cache, &[]);
    let (found, compiled) = counts(&built);
    assert!(found > 0 && compiled == 0, "{}", text(&built.stderr));
    assert_eq!(left(&built), 0, "{}", text(&built.stderr));
    assert_eq!(run(dir, "a"), NO_INLINE_OUTPUT);
}
