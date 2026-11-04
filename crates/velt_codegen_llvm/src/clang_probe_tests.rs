use std::collections::HashMap;
#[cfg(unix)]
use std::os::unix::fs::{symlink, PermissionsExt};
use std::sync::Mutex;

use super::*;

/// How long a clang that should answer gets: the limit isn't what those tests check.
#[cfg(unix)]
const AMPLE: Duration = Duration::from_secs(120);

fn usable(banner: &str) -> Option<bool> {
    parse_version(banner).map(|v| v.major >= v.min_major())
}

/// A memo held in memory. As the build cache does, it keeps the first value stored under a key.
#[derive(Default)]
struct Memo(Mutex<HashMap<Vec<u8>, Vec<u8>>>);

impl CodeCache for Memo {
    fn get(&self, key: &[u8]) -> Option<Vec<u8>> {
        self.0.lock().unwrap().get(key).cloned()
    }

    fn put(&self, key: &[u8], value: &[u8]) {
        let mut map = self.0.lock().unwrap();
        map.entry(key.to_vec()).or_insert_with(|| value.to_vec());
    }
}

/// A directory for a test's files, removed when it ends.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> TempDir {
        let dir = std::env::temp_dir().join(format!("velt-probe-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir.canonicalize().unwrap())
    }

    fn file(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn may_start(_path: &Path) {
    #[cfg(unix)]
    std::fs::set_permissions(_path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A file that looks like a native program by its first bytes, and can't run.
fn looks_native(path: &Path) {
    std::fs::write(path, b"\x7fELF not a program").unwrap();
    may_start(path);
}

/// The script `path` runs `body`.
#[cfg(unix)]
fn script(path: &Path, body: &str) {
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    may_start(path);
}

fn banner_of(probe: Probe) -> String {
    match probe {
        Probe::Usable { banner, .. } => banner,
        other => panic!("{other:?}"),
    }
}

/// With a memo, a clang whose answer is known is not started: a file that can't run counts as the
/// clang the memo says it is. Its key follows the file: another size is another key, so a replaced
/// clang is asked again.
#[test]
fn a_known_clang_is_not_started_again() {
    let tmp = TempDir::new("known");
    let fake = tmp.file("clang-fake.exe");
    looks_native(&fake);
    let probe = |memo: &Memo| probe_with(&fake, Some(memo), PROBE_LIMIT);
    let memo = Memo::default();
    assert!(
        matches!(probe(&memo), Probe::Missing),
        "nothing known: it is run"
    );
    let key = probe_key(&fake).unwrap();
    memo.put(
        &key,
        b"clang version 18.1.8 (https://github.com/llvm/llvm-project)\n",
    );
    assert!(
        matches!(probe(&memo), Probe::Usable { .. }),
        "known: not run"
    );
    let memo = Memo::default();
    memo.put(&key, b"clang version 14.0.0\n");
    assert!(matches!(probe(&memo), Probe::TooOld(_)));
    std::fs::write(&fake, b"\x7fELF another, longer file").unwrap();
    assert_ne!(probe_key(&fake).unwrap(), key, "replaced: asked again");
    assert!(probe_key(&tmp.file("missing.exe")).is_none());
}

/// A script stays the same while the clang behind it changes, so what it said last time says
/// nothing, and it is what keys the objects.
#[cfg(unix)]
#[test]
fn a_script_is_asked_each_time() {
    let memo = Memo::default();
    let tmp = TempDir::new("script");
    let wrapper = tmp.file("clang-29");
    script(&wrapper, "exec \"$(dirname \"$0\")/real\" \"$@\"");
    let real = tmp.file("real");
    script(&real, "echo clang version 97.0.0");
    let probe = || probe_with(&wrapper, Some(&memo), AMPLE);
    assert_eq!(banner_of(probe()), "clang version 97.0.0");
    script(&real, "echo clang version 98.0.0");
    assert_eq!(banner_of(probe()), "clang version 98.0.0");
    assert!(memo.0.lock().unwrap().is_empty(), "nothing kept");
    std::fs::remove_file(&real).unwrap();
    assert!(matches!(probe(), Probe::Missing), "what it starts has gone");
}

/// A link to a native program not named clang is a program that picks what to run by the name it
/// was started as (ccache, mise): the same. A link to a clang isn't.
#[cfg(unix)]
#[test]
fn a_link_to_another_program_is_asked_each_time() {
    let tmp = TempDir::new("link");
    looks_native(&tmp.file("ccache"));
    symlink(tmp.file("ccache"), tmp.file("clang")).unwrap();
    assert!(probe_key(&tmp.file("clang")).is_none(), "a dispatcher");
    looks_native(&tmp.file("clang-23"));
    symlink(tmp.file("clang-23"), tmp.file("cc")).unwrap();
    assert!(probe_key(&tmp.file("cc")).is_some(), "the clang itself");
}

#[test]
fn native_programs_are_told_by_their_first_bytes() {
    let tmp = TempDir::new("magic");
    let file = tmp.file("clang");
    let native = |bytes: &[u8]| {
        std::fs::write(&file, bytes).unwrap();
        is_native_program(&file)
    };
    assert!(native(&[0xcf, 0xfa, 0xed, 0xfe, 0]), "Mach-O");
    assert!(native(&[0xca, 0xfe, 0xba, 0xbe, 0]), "universal Mach-O");
    assert!(native(b"\x7fELF\x02"), "ELF");
    assert!(native(b"MZ\x90\0"), "PE");
    assert!(!native(b"#!/bin/sh\n"), "a script");
    assert!(!native(b"#!"), "too short");
    assert!(!native(b""), "empty");
    assert!(!is_native_program(&tmp.file("missing")));
}

/// A clang that hangs is killed at the limit, and the answer isn't kept. `exec`, so that it is the
/// process killed, and `tail -f`, which blocks and doesn't sleep any set time.
#[cfg(unix)]
#[test]
fn a_clang_that_hangs_times_out() {
    let memo = Memo::default();
    let tmp = TempDir::new("hang");
    let clang = tmp.file("clang-30");
    script(&clang, "exec tail -f /dev/null");
    let limit = Duration::from_millis(100);
    assert!(matches!(
        probe_with(&clang, Some(&memo), limit),
        Probe::TimedOut
    ));
    assert!(matches!(
        try_compile(&clang, &[], limit),
        Err(Failure::Other(_))
    ));
    assert!(compiles_with(&clang, "b", Some(&memo), limit).is_err());
    assert!(memo.0.lock().unwrap().is_empty(), "it may answer next time");
}

/// A clang that closes its pipes and hangs is caught too, not only one that keeps them open.
#[cfg(unix)]
#[test]
fn a_clang_that_closes_its_output_and_hangs_times_out() {
    let tmp = TempDir::new("closed");
    let clang = tmp.file("clang-30");
    script(&clang, "exec >/dev/null 2>&1\nexec tail -f /dev/null");
    let limit = Duration::from_millis(100);
    assert!(matches!(probe_with(&clang, None, limit), Probe::TimedOut));
}

/// The check compiles IR from stdin, and says what clang said when it can't.
#[cfg(unix)]
#[test]
fn the_target_check_compiles_and_reports() {
    let tmp = TempDir::new("check");
    let clang = tmp.file("clang-29");
    script(
        &clang,
        "echo\necho 'error: no available targets' >&2\necho more >&2\nexit 1",
    );
    let err = compiles_with(&clang, "b", None, AMPLE).unwrap_err();
    assert_eq!(err, "error: no available targets");
    script(
        &clang,
        "input=$(cat)\ncase \"$input\" in *velt_clang_check*) exit 0;; esac\nexit 1",
    );
    assert_eq!(compiles_with(&clang, "b", None, AMPLE), Ok(()));
}

/// Same file and banner: the verdict stands, whichever it was. Another banner is asked again.
#[cfg(unix)]
#[test]
fn a_known_verdict_is_not_asked_again() {
    let tmp = TempDir::new("verdict");
    let clang = tmp.file("clang-29");
    let log = tmp.file("log");
    script(
        &clang,
        &format!("echo x >> '{}'\ncat >/dev/null\nexit 1", log.display()),
    );
    let starts = || std::fs::read_to_string(&log).map_or(0, |t| t.lines().count());
    let memo = Memo::default();
    let check = |banner| compiles_with(&clang, banner, Some(&memo), AMPLE);
    assert!(check("one").is_err());
    assert_eq!(starts(), 1);
    assert!(check("one").is_err());
    assert_eq!(starts(), 1, "known: not started");
    assert!(check("two").is_err());
    assert_eq!(starts(), 2, "another version: asked");
    let key = check_key(&clang, "three").unwrap();
    memo.put(&key, COMPILES);
    assert_eq!(check("three"), Ok(()), "a pass is kept as well");
    assert_eq!(starts(), 2);
}

/// A clang killed by a signal (`dyld` failing to find a library aborts) said nothing about itself:
/// the next build asks again.
#[cfg(unix)]
#[test]
fn a_crash_in_the_target_check_is_not_kept() {
    let memo = Memo::default();
    let tmp = TempDir::new("crash");
    let clang = tmp.file("clang-29");
    script(&clang, "kill -9 $$");
    assert!(compiles_with(&clang, "b", Some(&memo), AMPLE).is_err());
    assert!(memo.0.lock().unwrap().is_empty());
}

/// A clang that failed a build is asked again: what it needs may have gone while the file stayed.
/// The new answer is then the one kept, so a clang that is well again costs no more than before.
#[test]
fn a_clang_that_failed_is_forgotten() {
    let memo = Memo::default();
    let tmp = TempDir::new("forget");
    let clang = tmp.file("clang-23.exe");
    looks_native(&clang);
    memo.put(&probe_key(&clang).unwrap(), b"clang version 23.1.4\n");
    let banner = "clang version 23.1.4";
    let key = check_key(&clang, banner).unwrap();
    memo.put(&key, COMPILES);
    let probe = || probe_with(&clang, Some(&memo), PROBE_LIMIT);
    let check = || compiles_with(&clang, banner, Some(&memo), PROBE_LIMIT);
    assert!(matches!(probe(), Probe::Usable { .. }), "known: not run");
    assert_eq!(check(), Ok(()));
    forget_in(&clang, banner, Some(&memo));
    assert!(matches!(probe(), Probe::Missing), "asked: it doesn't run");
    assert!(check().is_err(), "asked: it doesn't compile");
    let now = current(probe_key(&clang).unwrap(), &memo);
    memo.put(&now, b"clang version 23.1.5\n");
    assert_eq!(banner_of(probe()), "clang version 23.1.5", "kept again");
    forget_in(&clang, banner, Some(&memo));
    assert!(matches!(probe(), Probe::Missing), "and can go again");
}

/// As `execvp` does: a file that can't be started is passed by, there or named.
#[cfg(unix)]
#[test]
fn a_name_resolves_to_the_file_that_would_start() {
    let tmp = TempDir::new("resolve");
    let (a, b) = (tmp.file("a"), tmp.file("b"));
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(b.join("clang-dir/clang")).unwrap();
    std::fs::write(a.join("clang"), "").unwrap();
    script(&b.join("clang"), "");
    let path = std::env::join_paths([&a, &b.join("clang-dir"), &b]).unwrap();
    let name = Path::new("clang");
    assert_eq!(resolve_in(name, Some(&path)), Some(b.join("clang")));
    let only_a = std::env::join_paths([&a]).unwrap();
    assert_eq!(resolve_in(name, Some(&only_a)), None);
    assert_eq!(resolve_in(&a.join("clang"), None), None, "named");
    assert_eq!(resolve_in(&b.join("clang"), None), Some(b.join("clang")));
}

/// A file with an execute bit this user doesn't have doesn't hide the clang behind it: only the
/// others' bit is set, and the owner's bits are the ones that apply to the owner. Root may start it.
#[cfg(unix)]
#[test]
fn a_file_only_others_can_start_does_not_hide_the_next() {
    use std::os::unix::fs::PermissionsExt;
    // SAFETY: geteuid has no preconditions and cannot fail.
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let tmp = TempDir::new("others");
    let (a, b) = (tmp.file("a"), tmp.file("b"));
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    script(&a.join("clang"), "");
    script(&b.join("clang"), "");
    std::fs::set_permissions(a.join("clang"), std::fs::Permissions::from_mode(0o001)).unwrap();
    let path = std::env::join_paths([&a, &b]).unwrap();
    assert_eq!(
        resolve_in(Path::new("clang"), Some(&path)),
        Some(b.join("clang"))
    );
}

/// An update behind the system clang's shim changes the shim's key: otherwise the old version
/// would keep being answered, and be what keys the objects.
#[test]
fn a_new_clang_behind_the_shim_is_asked_again() {
    let tmp = TempDir::new("shim");
    let (active, other) = (tmp.file("active"), tmp.file("other"));
    let clang = active.join("usr/bin/clang");
    std::fs::create_dir_all(clang.parent().unwrap()).unwrap();
    std::fs::write(&clang, b"one").unwrap();
    let shim = tmp.file("shim");
    std::fs::write(&shim, b"shim").unwrap();
    let key = file_key(&shim, std::slice::from_ref(&active)).unwrap();
    assert_eq!(file_key(&shim, std::slice::from_ref(&active)).unwrap(), key);
    std::fs::write(&clang, b"newer").unwrap();
    assert_ne!(file_key(&shim, &[active]).unwrap(), key, "tools updated");
    assert_ne!(file_key(&shim, &[other]).unwrap(), key, "another selected");
}

/// The system clang's key is the one that looks behind the shim.
#[cfg(target_os = "macos")]
#[test]
fn the_system_clang_is_keyed_by_what_is_behind_it() {
    let Some(key) = probe_key(Path::new(SYSTEM_CLANG)) else {
        return;
    };
    let tools = b"/Library/Developer/CommandLineTools";
    assert!(key.windows(tools.len()).any(|w| w == tools));
}

#[test]
fn version_banners() {
    let win = "clang version 22.1.8 (https://github.com/llvm/llvm-project ca7933e)\nTarget: x";
    assert_eq!(usable(win), Some(true));
    assert_eq!(
        usable("Ubuntu clang version 18.1.8 (11~20.04.2)"),
        Some(true)
    );
    assert_eq!(usable("Ubuntu clang version 16.0.6 (++2023)"), Some(true));
    // Ubuntu 22.04's default clang: no `memory(...)` attributes yet.
    assert_eq!(
        usable("Ubuntu clang version 14.0.0-1ubuntu1.1"),
        Some(false)
    );
    assert_eq!(usable("clang version 10.0.0-4ubuntu1"), Some(false));
    // Apple clang 15 is LLVM 16; Apple clang 14 is LLVM 15.
    assert_eq!(
        usable("Apple clang version 15.0.0 (clang-1500.3.9.4)"),
        Some(true)
    );
    assert_eq!(
        usable("Apple clang version 14.0.3 (clang-1403.0.22.14.1)"),
        Some(false)
    );
    assert_eq!(
        parse_version("Ubuntu clang version 14.0.0-1ubuntu1.1")
            .unwrap()
            .describe(),
        "clang 14.0.0-1ubuntu1.1"
    );
    assert_eq!(usable("gcc (GCC) 13.2.0"), None);
}

/// Versions compare as numbers, whatever a distribution appends.
#[test]
fn version_numbers() {
    let number = |banner: &str| parse_version(banner).unwrap().number;
    assert_eq!(number("Homebrew clang version 23.1.4"), (23, 1, 4));
    assert_eq!(number("Ubuntu clang version 14.0.0-1ubuntu1.1"), (14, 0, 0));
    assert_eq!(number("clang version 20.1.0git (x)"), (20, 1, 0));
    assert_eq!(number("clang version 19"), (19, 0, 0));
    assert!(number("clang version 23.10.0") > number("clang version 23.9.0"));
}
