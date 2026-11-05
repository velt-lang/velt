//! What a build pays for a candidate that hangs: [`Waits`]. Times and limits are the tests'.

use std::cell::RefCell;

use super::*;

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
        hung_key(&tmp.file("clang"), Asked::Version),
        hung_key(&tmp.file("clang-29"), Asked::Version)
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

/// A clang that answers its version and hangs on a compile costs the same, so it counts the same,
/// in the order a build asks: its answer to `--version` doesn't end the count of the check. The
/// version gets an ample limit, so that a load can't make it the one that times out.
#[cfg(unix)]
#[test]
fn a_clang_whose_target_check_hung_is_left_alone_too() {
    let hanging = Hanging::new("hung-check");
    let real = hanging.tmp.file("real");
    let log = hanging.tmp.file("log");
    script(
        &real,
        &format!(
            "echo \"$1\" >> '{}'\n[ \"$1\" = --version ] && {{ echo clang version 97.0.0; exit 0; \
             }}\nexec tail -f /dev/null",
            log.display()
        ),
    );
    let told = RefCell::new(vec![]);
    let tell = |line: &str| told.borrow_mut().push(line.to_string());
    // What a build does at `now`: `None` if the clang was left alone, else what the check said.
    let build = |now| match hanging.answering(now, true) {
        Probe::Usable { banner, .. } => Some(hanging.at(now, &tell).check(&hanging.clang, &banner)),
        Probe::Held => None,
        other => panic!("{other:?}"),
    };
    let starts = || std::fs::read_to_string(&log).unwrap().lines().count();
    assert!(build(WROTE - 1).unwrap().is_err());
    assert_eq!(starts(), 2, "its version and the check");
    assert!(build(WROTE).unwrap().is_err(), "asked again");
    assert_eq!(starts(), 4);
    {
        let told = told.borrow();
        assert_eq!(told.len(), 2, "{told:?}");
        assert!(told[0].contains("a small compile"), "{told:?}");
        assert!(told[0].contains("the next build asks it again"), "{told:?}");
        assert!(told[1].contains("for an hour"), "{told:?}");
    }
    assert!(build(WROTE + 1).is_none(), "left alone");
    assert!(build(WROTE + HUNG_HOLD - 1).is_none(), "for the hour");
    assert_eq!(starts(), 4, "not started");
    assert_eq!(told.borrow().len(), 2, "no wait, nothing to say");
    assert!(build(WROTE + HUNG_HOLD).unwrap().is_err(), "then asked");
    assert_eq!(starts(), 6);
}

/// `velt doctor` asks a clang left alone for its target check, and when the check answers, that
/// ends it for builds too.
#[cfg(unix)]
#[test]
fn doctor_asks_a_clang_whose_target_check_hung_and_its_answer_counts() {
    let hanging = Hanging::new("doctor-check");
    let real = hanging.tmp.file("real");
    script(
        &real,
        "[ \"$1\" = --version ] && { echo clang version 97.0.0; exit 0; }\n\
         exec tail -f /dev/null",
    );
    let check = |now, limit| {
        let waits = Waits {
            limit,
            ..hanging.at(now, &|_| {})
        };
        waits.check(&hanging.clang, "clang version 97.0.0")
    };
    assert!(check(WROTE - 1, SHORT).is_err());
    assert!(check(WROTE, SHORT).is_err());
    assert!(matches!(hanging.answering(WROTE + 1, true), Probe::Held));
    let doctor = hanging.answering(WROTE + 2, false);
    assert_eq!(banner_of(doctor), "clang version 97.0.0");
    assert!(
        matches!(hanging.answering(WROTE + 3, true), Probe::Held),
        "a version isn't an answer to the check"
    );
    script(
        &real,
        "[ \"$1\" = --version ] && { echo clang version 97.0.0; exit 0; }\ncat >/dev/null",
    );
    assert_eq!(check(WROTE + 4, AMPLE), Ok(()));
    let build = hanging.answering(WROTE + 5, true);
    assert_eq!(banner_of(build), "clang version 97.0.0", "no longer held");
}
