#[cfg(unix)]
use std::cell::RefCell;
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

/// Write the program `path` from another process. A file a program in this process holds open for
/// writing can't be started (`ETXTBSY`), and a process another test thread is starting holds a copy
/// of every descriptor until its `exec`, so a script written here can fail to start for that moment.
#[cfg(unix)]
fn write_program(path: &Path, text: &str) {
    let mut writer = Command::new("/bin/sh")
        .args(["-c", "cat > \"$0\" && chmod 755 \"$0\""])
        .arg(path)
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = writer.stdin.take().unwrap();
    stdin.write_all(text.as_bytes()).unwrap();
    drop(stdin);
    assert!(writer.wait().unwrap().success());
}

/// The script `path` runs `body`.
#[cfg(unix)]
fn script(path: &Path, body: &str) {
    write_program(path, &format!("#!/bin/sh\n{body}\n"));
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
        Err(Failure::TimedOut(_))
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

/// A limit to see a hanging clang at, and a time in the middle of an hour: `HOUR + HUNG_HOLD / 2 + x`
/// is another hour number, with the note still in force.
#[cfg(unix)]
const SHORT: Duration = Duration::from_millis(100);
#[cfg(unix)]
const WROTE: u64 = 10 * HUNG_HOLD + HUNG_HOLD / 2;

/// A candidate that blocks, asked at times of the test's choosing, and what it told.
#[cfg(unix)]
struct Hanging {
    tmp: TempDir,
    clang: PathBuf,
    memo: Memo,
    told: RefCell<Vec<String>>,
}

#[cfg(unix)]
impl Hanging {
    /// A wrapper, whose file stays the same whatever `real` does, in front of a clang that hangs.
    fn new(name: &str) -> Hanging {
        let tmp = TempDir::new(name);
        let clang = tmp.file("clang-29");
        script(&clang, "exec \"$(dirname \"$0\")/real\" \"$@\"");
        script(&tmp.file("real"), "exec tail -f /dev/null");
        Hanging {
            tmp,
            clang,
            memo: Memo::default(),
            told: RefCell::default(),
        }
    }

    fn at<'a>(&'a self, now: u64, tell: &'a dyn Fn(&str)) -> Waits<'a> {
        Waits {
            memo: Some(&self.memo),
            limit: SHORT,
            now: Some(now),
            tell,
        }
    }

    /// What a build's search gets at `now`, while the clang hangs.
    fn build(&self, now: u64) -> Probe {
        let tell = |line: &str| self.told.borrow_mut().push(line.to_string());
        self.at(now, &tell).version(&self.clang, true)
    }

    /// What `velt doctor` gets at `now`, while the clang hangs.
    fn doctor(&self, now: u64) -> Probe {
        self.at(now, &|_| {}).version(&self.clang, false)
    }

    /// As [`Self::build`] or, if not `hold`, [`Self::doctor`], once it answers: the limit is ample.
    fn answering(&self, now: u64, hold: bool) -> Probe {
        let waits = Waits {
            limit: AMPLE,
            ..self.at(now, &|_| {})
        };
        waits.version(&self.clang, hold)
    }

    fn take_told(&self) -> Vec<String> {
        self.told.borrow_mut().drain(..).collect()
    }
}

/// A clang that hangs costs two waits with a line each, on two builds running. For an hour after
/// the second it isn't asked, in that hour and the next, and then it is, twice again. No clock runs:
/// the times are the test's.
#[cfg(unix)]
#[test]
fn a_clang_that_hung_twice_is_left_alone_for_a_while() {
    let hanging = Hanging::new("hold");
    assert!(matches!(hanging.build(WROTE - 10), Probe::TimedOut));
    let told = hanging.take_told();
    assert_eq!(told.len(), 1, "{told:?}");
    assert!(
        told[0].contains(&*hanging.clang.to_string_lossy()),
        "{told:?}"
    );
    assert!(told[0].contains("--version"), "{told:?}");
    assert!(told[0].contains("the next build asks it again"), "{told:?}");
    assert!(
        matches!(hanging.build(WROTE), Probe::TimedOut),
        "asked again"
    );
    let told = hanging.take_told();
    assert_eq!(told.len(), 1, "{told:?}");
    assert!(told[0].contains("for an hour"), "{told:?}");
    assert!(told[0].contains("velt doctor"), "{told:?}");
    for later in [1, HUNG_HOLD / 2 - 1, HUNG_HOLD / 2, HUNG_HOLD - 1] {
        let probe = hanging.build(WROTE + later);
        assert!(matches!(probe, Probe::Held), "{later} s on: {probe:?}");
    }
    assert!(hanging.take_told().is_empty(), "no wait, nothing to say");
    assert!(matches!(hanging.build(WROTE + HUNG_HOLD), Probe::TimedOut));
    assert!(matches!(
        hanging.build(WROTE + HUNG_HOLD + 1),
        Probe::TimedOut
    ));
    assert_eq!(hanging.take_told().len(), 2);
    assert!(matches!(hanging.build(WROTE + HUNG_HOLD + 2), Probe::Held));
}

/// A first start can take longer than the limit (macOS checks a new executable), and the next is
/// quick. One timeout must not cost a clang that works its place for the hour: the next build asks,
/// its answer counts, and a timeout after that is a first one again.
#[cfg(unix)]
#[test]
fn a_clang_that_was_slow_once_is_asked_again_and_used() {
    let hanging = Hanging::new("slow-once");
    assert!(matches!(hanging.build(WROTE), Probe::TimedOut));
    script(&hanging.tmp.file("real"), "echo clang version 97.0.0");
    let answer = hanging.answering(WROTE + 1, true);
    assert_eq!(banner_of(answer), "clang version 97.0.0", "the next build");
    script(&hanging.tmp.file("real"), "exec tail -f /dev/null");
    assert!(matches!(hanging.build(WROTE + 2), Probe::TimedOut));
    let told = hanging.take_told();
    assert!(told[1].contains("the next build asks it again"), "{told:?}");
    assert!(
        matches!(hanging.build(WROTE + 3), Probe::TimedOut),
        "not held"
    );
    assert!(matches!(hanging.build(WROTE + 4), Probe::Held));
}

/// Two timeouts count together only within the hour: builds further apart aren't what costs.
#[cfg(unix)]
#[test]
fn timeouts_hours_apart_do_not_add_up() {
    let hanging = Hanging::new("apart");
    assert!(matches!(hanging.build(WROTE), Probe::TimedOut));
    assert!(matches!(hanging.build(WROTE + HUNG_HOLD), Probe::TimedOut));
    assert!(matches!(
        hanging.build(WROTE + 2 * HUNG_HOLD),
        Probe::TimedOut
    ));
    let told = hanging.take_told();
    assert_eq!(told.len(), 3, "{told:?}");
    assert!(told.iter().all(|l| !l.contains("for an hour")), "{told:?}");
}

/// A clock that went back past a note doesn't make it last: a note from the future doesn't count,
/// in that hour or the one before, and nothing promises what can't be kept.
#[cfg(unix)]
#[test]
fn a_clock_that_went_back_does_not_extend_the_wait() {
    let hanging = Hanging::new("clock");
    assert!(matches!(hanging.build(WROTE - 1), Probe::TimedOut));
    assert!(matches!(hanging.build(WROTE), Probe::TimedOut));
    assert!(matches!(hanging.build(WROTE + 1), Probe::Held), "held");
    hanging.take_told();
    assert!(
        matches!(hanging.build(WROTE - 2), Probe::TimedOut),
        "same hour"
    );
    let told = hanging.take_told();
    assert!(!told[0].contains("for an hour"), "{told:?}");
    let long_ago = WROTE - 5 * HUNG_HOLD;
    assert!(
        matches!(hanging.build(long_ago), Probe::TimedOut),
        "no note"
    );
}

/// `velt doctor` asks all the same, and says nothing of waiting: it reports. An answer ends the
/// hold for the builds after it.
#[cfg(unix)]
#[test]
fn doctor_asks_a_clang_that_hung_and_its_answer_counts() {
    let hanging = Hanging::new("doctor");
    assert!(matches!(hanging.doctor(WROTE - 1), Probe::TimedOut));
    assert!(matches!(hanging.doctor(WROTE), Probe::TimedOut));
    assert!(hanging.take_told().is_empty());
    assert!(
        matches!(hanging.build(WROTE + 1), Probe::Held),
        "doctor's notes"
    );
    script(&hanging.tmp.file("real"), "echo clang version 97.0.0");
    assert!(matches!(hanging.build(WROTE + 2), Probe::Held), "unasked");
    let answer = hanging.answering(WROTE + 3, false);
    assert_eq!(banner_of(answer), "clang version 97.0.0");
    let answer = hanging.answering(WROTE + 4, true);
    assert_eq!(banner_of(answer), "clang version 97.0.0", "taken back");
    script(&hanging.tmp.file("real"), "exec tail -f /dev/null");
    assert!(matches!(hanging.build(WROTE + 5), Probe::TimedOut));
    assert!(matches!(hanging.build(WROTE + 6), Probe::TimedOut));
    assert!(
        matches!(hanging.build(WROTE + 7), Probe::Held),
        "and can hang again"
    );
}

/// The note is the file's: a clang replaced is asked. And each name of a dispatcher has its own.
#[cfg(unix)]
#[test]
fn a_new_file_is_asked_though_the_old_one_hung() {
    let hanging = Hanging::new("replaced");
    assert!(matches!(hanging.build(WROTE - 1), Probe::TimedOut));
    assert!(matches!(hanging.build(WROTE), Probe::TimedOut));
    assert!(matches!(hanging.build(WROTE + 1), Probe::Held));
    script(
        &hanging.clang,
        "echo clang version 97.0.0 # another file, of another size",
    );
    let answer = hanging.answering(WROTE + 2, true);
    assert_eq!(banner_of(answer), "clang version 97.0.0");
    let tmp = TempDir::new("names");
    looks_native(&tmp.file("mise"));
    symlink(tmp.file("mise"), tmp.file("clang")).unwrap();
    symlink(tmp.file("mise"), tmp.file("clang-29")).unwrap();
    assert_ne!(
        hung_key(&tmp.file("clang")),
        hung_key(&tmp.file("clang-29"))
    );
}

/// With nowhere to keep it, or no clock, each build waits, says so, and promises nothing.
#[cfg(unix)]
#[test]
fn without_a_cache_or_a_clock_each_build_waits_and_says_so() {
    let hanging = Hanging::new("nowhere");
    let told = RefCell::new(vec![]);
    let tell = |line: &str| told.borrow_mut().push(line.to_string());
    let no_cache = Waits {
        memo: None,
        ..hanging.at(WROTE, &tell)
    };
    let no_clock = Waits {
        now: None,
        ..hanging.at(WROTE, &tell)
    };
    for waits in [no_cache, no_clock] {
        for _ in 0..2 {
            let probe = waits.version(&hanging.clang, true);
            assert!(matches!(probe, Probe::TimedOut), "{probe:?}");
        }
    }
    let told = told.into_inner();
    assert_eq!(told.len(), 4, "{told:?}");
    assert!(told.iter().all(|l| !l.contains("for an hour")), "{told:?}");
    assert!(hanging.memo.0.lock().unwrap().is_empty());
}

/// A clang that answers its version and hangs on a compile costs the same, so it counts the same.
#[cfg(unix)]
#[test]
fn a_clang_whose_target_check_hung_is_left_alone_too() {
    let hanging = Hanging::new("hung-check");
    let told = RefCell::new(vec![]);
    let tell = |line: &str| told.borrow_mut().push(line.to_string());
    assert!(hanging
        .at(WROTE - 1, &tell)
        .check(&hanging.clang, "b")
        .is_err());
    assert!(hanging.at(WROTE, &tell).check(&hanging.clang, "b").is_err());
    let told = told.into_inner();
    assert!(told[0].contains("a small compile"), "{told:?}");
    assert!(told[1].contains("for an hour"), "{told:?}");
    assert!(matches!(hanging.build(WROTE + 1), Probe::Held));
}

/// A file being written can't be started (`ETXTBSY` where the system says so). That is no verdict
/// on the clang: nothing of it is kept, and when the writing is done it counts. The writer is another
/// process, so that this one keeps no descriptor that other test threads could copy.
#[cfg(unix)]
#[test]
fn a_clang_being_written_is_asked_again() {
    let memo = Memo::default();
    let tmp = TempDir::new("busy");
    let clang = tmp.file("clang-29");
    script(&clang, "cat >/dev/null\necho clang version 97.0.0");
    let mut writer = Command::new("/bin/sh")
        .args(["-c", "exec 3>>\"$0\"; echo ready; read _; exit 0"])
        .arg(&clang)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut ready = [0u8; 6];
    writer
        .stdout
        .take()
        .unwrap()
        .read_exact(&mut ready)
        .unwrap();
    let busy = probe_with(&clang, Some(&memo), AMPLE);
    let check = compiles_with(&clang, "b", Some(&memo), AMPLE);
    if cfg!(target_os = "linux") {
        assert!(matches!(busy, Probe::Missing), "{busy:?}");
        assert!(check.is_err(), "{check:?}");
    }
    if check.is_err() {
        assert!(memo.0.lock().unwrap().is_empty(), "nothing kept");
    }
    drop(writer.stdin.take());
    assert!(writer.wait().unwrap().success());
    assert_eq!(
        banner_of(probe_with(&clang, Some(&memo), AMPLE)),
        "clang version 97.0.0"
    );
    assert_eq!(compiles_with(&clang, "b", Some(&memo), AMPLE), Ok(()));
}

/// A shell's 126 and 127 are what a wrapper exits with when what it starts can't be started (being
/// written, gone for now). clang didn't say no, so it isn't kept as if it had.
#[cfg(unix)]
#[test]
fn a_wrapper_that_could_not_start_its_clang_is_not_a_verdict() {
    let memo = Memo::default();
    let tmp = TempDir::new("exit126");
    let clang = tmp.file("clang-29");
    for code in [126, 127] {
        script(&clang, &format!("echo cannot start >&2\nexit {code}"));
        assert!(compiles_with(&clang, "b", Some(&memo), AMPLE).is_err());
        assert!(memo.0.lock().unwrap().is_empty(), "{code}");
    }
    script(&clang, "echo no >&2\nexit 1");
    assert!(compiles_with(&clang, "b", Some(&memo), AMPLE).is_err());
    assert_eq!(memo.0.lock().unwrap().len(), 1, "clang's own verdict lasts");
}

/// A candidate that writes without end doesn't fill the memory: its first 64 KiB are kept, and the
/// rest read, so that it ends as it would have and isn't told its reader went away. `head` fails
/// if it is, and the script's status is `head`'s.
#[cfg(unix)]
#[test]
fn only_so_much_of_a_candidates_output_is_kept() {
    let tmp = TempDir::new("flood");
    let clang = tmp.file("clang-29");
    script(
        &clang,
        "echo clang version 97.0.0\nyes | head -c 1000000 && exit 0\nexit 9",
    );
    let mut cmd = Command::new(&clang);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    match run_limited(cmd, AMPLE) {
        Ran::Exited(status, text) => {
            assert!(status.success(), "{status}");
            assert_eq!(text.len() as u64, MOST_OUTPUT);
            assert!(text.starts_with(b"clang version 97.0.0\n"));
        }
        _ => panic!("it should have ended"),
    }
    assert_eq!(
        banner_of(probe_with(&clang, None, AMPLE)),
        "clang version 97.0.0"
    );
}
