use std::cell::RefCell;

#[cfg(unix)]
use std::os::unix::fs::symlink;

use crate::clang_probe::judge;

use super::*;

/// The candidates of a machine: what each answers to `--version`, `None` when it doesn't run.
fn machine<'a>(answers: &'a [(&'a str, Option<&'a str>)]) -> impl Fn(&Path) -> Probe + 'a {
    move |clang: &Path| {
        let name = clang.to_str().unwrap();
        let answer = answers.iter().find(|(n, _)| *n == name);
        match answer.unwrap_or_else(|| panic!("`{name}` was asked")).1 {
            Some(text) => judge(text),
            None => Probe::Missing,
        }
    }
}

/// What the search can ask of `machine`, on which everything compiles.
fn all_compile<'a>(machine: &'a dyn Fn(&Path) -> Probe) -> Ask<'a> {
    Ask {
        version: machine,
        compiles: &|_, _| Ok(()),
    }
}

fn paths(answers: &[(&str, Option<&str>)]) -> Vec<PathBuf> {
    answers.iter().map(|(n, _)| PathBuf::from(n)).collect()
}

const SYSTEM: &str = "/usr/bin/clang";
const APPLE: Option<&str> = Some("Apple clang version 21.0.0 (clang-2100.3.34.2)\nTarget: x\n");
const BREW: Option<&str> = Some("Homebrew clang version 23.1.4\nTarget: x\n");

fn chosen(search: &Search) -> &str {
    search.found.as_ref().unwrap().path.to_str().unwrap()
}

/// `candidates`, ranked, with `/usr/bin/clang` answering `system` as the last resort.
fn newest_of(candidates: &[(&str, Option<&str>)], system: Option<&str>) -> Search {
    let mut all = candidates.to_vec();
    all.push((SYSTEM, system));
    let probe = machine(&all);
    newest(paths(candidates), Some(SYSTEM.into()), &all_compile(&probe))
}

#[test]
fn the_newest_clang_wins_wherever_it_is_in_the_list() {
    let search = newest_of(
        &[
            ("a/clang", Some("clang version 18.1.8")),
            ("b/clang", BREW),
            ("c/clang-23", Some("clang version 23.1.2")),
            ("d/clang-22", Some("Debian clang version 22.9.9 (3+b1)")),
        ],
        APPLE,
    );
    assert_eq!(chosen(&search), "b/clang");
    assert_eq!(search.why, Why::Newest);
    let text = search.describe().unwrap();
    assert!(text.contains("the newest clang found"), "{text}");
    assert!(text.contains("also found a/clang (clang 18.1.8)"), "{text}");
}

#[test]
fn of_two_clangs_of_one_version_the_earlier_is_kept() {
    let search = newest_of(&[("a/clang", BREW), ("b/clang-23", BREW)], APPLE);
    assert_eq!(chosen(&search), "a/clang");
}

/// The system clang is the last resort, and is not started when another clang will do: the
/// machine panics when it is asked for something this list doesn't have.
#[test]
fn the_system_clang_is_not_asked_when_llvm_is_found() {
    let answers = [("b/clang", BREW)];
    let probe = machine(&answers);
    let search = newest(paths(&answers), Some(SYSTEM.into()), &all_compile(&probe));
    assert_eq!(chosen(&search), "b/clang");
    assert!(search
        .describe()
        .unwrap()
        .contains("/usr/bin/clang is the last resort"));
}

#[test]
fn the_system_clang_is_used_alone_and_doctor_says_how_to_get_a_newer_one() {
    let search = newest_of(&[], APPLE);
    assert_eq!(chosen(&search), SYSTEM);
    assert_eq!(search.why, Why::OnlySystem);
    let text = search.describe().unwrap();
    assert!(
        text.starts_with("/usr/bin/clang (Apple clang 21.0.0): no other usable"),
        "{text}"
    );
    assert!(text.contains("brew install llvm"), "{text}");
}

/// The check that decides this: without the minimum, LLVM 14 outranks Apple's clang.
#[test]
fn a_too_old_clang_falls_back_to_the_next() {
    let old = Some("clang version 14.0.0");
    let search = newest_of(&[("a/clang-30", old)], APPLE);
    assert_eq!(chosen(&search), SYSTEM, "not the too-old LLVM");
    assert!(
        search.rejected.is_none(),
        "not reported as rejected: {search:?}"
    );
    let text = search.describe().unwrap();
    assert!(text.contains("`a/clang-30` is clang 14.0.0, but"), "{text}");
    let search = newest_of(
        &[
            ("a/clang-30", old),
            ("b/clang-19", Some("clang version 19.1.7")),
        ],
        APPLE,
    );
    assert_eq!(chosen(&search), "b/clang-19");
}

#[test]
fn a_clang_that_does_not_run_falls_back_to_the_next() {
    let search = newest_of(&[("a/clang-30", None), ("b/clang", BREW)], None);
    assert_eq!(chosen(&search), "b/clang");
    let text = search.describe().unwrap();
    assert!(text.contains("`a/clang-30` does not run"), "{text}");
    let search = newest_of(&[("a/clang-30", None)], APPLE);
    assert_eq!(chosen(&search), SYSTEM);
}

/// With no clang left, the first too-old one is what the build's message names.
#[test]
fn only_too_old_clangs_are_reported_as_rejected() {
    let search = newest_of(
        &[("a/clang", Some("clang version 14.0.0"))],
        Some("Apple clang version 14.0.3 (clang-1403.0.22.14.1)"),
    );
    assert!(search.found.is_none());
    assert!(search.describe().is_none());
    assert!(search
        .rejected
        .unwrap()
        .contains("`a/clang` is clang 14.0.0"));
}

/// Apple's numbers aren't LLVM's: Apple clang 21 doesn't beat LLVM 19 on them, and an unrecognized
/// banner is only used when there is nothing else.
#[test]
fn origins_rank_before_versions() {
    let llvm = Some("clang version 19.1.7");
    let search = newest_of(&[("a/clang", APPLE), ("b/clang", llvm)], APPLE);
    assert_eq!(chosen(&search), "b/clang");
    let search = newest_of(
        &[("a/clang", Some("some wrapper 99")), ("b/clang", APPLE)],
        APPLE,
    );
    assert_eq!(chosen(&search), "b/clang");
    let search = newest_of(&[("a/clang", Some("some wrapper 99"))], None);
    assert_eq!(chosen(&search), "a/clang");
    assert_eq!(search.why, Why::OnlyUnknown);
    let text = search.describe().unwrap();
    assert!(text.contains("version unknown"), "{text}");
    assert!(!text.contains("newest"), "nothing says it is new: {text}");
}

#[test]
fn the_explicit_clang_is_final_and_says_so() {
    let old = [("x/clang", Some("clang version 14.0.0"))];
    let probe = machine(&old);
    let search = first_usable(paths(&old), Why::Explicit, None, &all_compile(&probe));
    assert!(search.found.is_none(), "no fallback from $VELT_CLANG");
    assert!(search
        .rejected
        .unwrap()
        .contains("`x/clang` is clang 14.0.0"));
    let good = [("x/clang", BREW)];
    let probe = machine(&good);
    let search = first_usable(paths(&good), Why::Explicit, None, &all_compile(&probe));
    let text = search.describe().unwrap();
    assert!(
        text.ends_with("(clang 23.1.4): set by $VELT_CLANG"),
        "{text}"
    );
}

/// Outside macOS the order decides: a new `clang-NN` does not overrule `clang`.
#[test]
fn the_fixed_order_keeps_the_first_usable_clang() {
    let answers = [
        ("a/clang", Some("clang version 14.0.0")),
        ("b/clang", Some("clang version 18.1.8")),
        ("c/clang-23", BREW),
    ];
    let probe = machine(&answers);
    let search = first_usable(
        paths(&answers),
        Why::FirstInOrder,
        None,
        &all_compile(&probe),
    );
    assert_eq!(chosen(&search), "b/clang");
    let text = search.describe().unwrap();
    assert!(
        text.contains("the first usable clang in the search order"),
        "{text}"
    );
    assert!(text.contains("`a/clang` is clang 14.0.0, but"), "{text}");
}

/// A file reached twice (Homebrew's `opt/llvm` also on `PATH`) is one candidate, the system clang
/// is none, and names not on `PATH` aren't candidates (no failed process start per build).
#[cfg(unix)]
#[test]
fn ranked_candidates_are_files_each_once() {
    let tmp = TempDir::new("candidates");
    let (one, two) = (tmp.subdir("one"), tmp.subdir("two"));
    program(&one.join("clang-23"));
    symlink(one.join("clang-23"), two.join("clang")).unwrap();
    program(&two.join("clang-19"));
    program(&two.join("clang-system"));
    symlink(two.join("clang-system"), one.join("clang-22")).unwrap();
    let path = std::env::join_paths([&one, &two]).unwrap();
    let list = ranked_candidates(Some(&path), &two.join("clang-system"));
    assert_eq!(tmp.within(list), [two.join("clang"), two.join("clang-19")]);
}

/// A link to a program not named clang may pick what to run by its own name, as ccache's do: each
/// name is a clang of its own. The same clang under two names is still one.
#[cfg(unix)]
#[test]
fn links_to_a_dispatcher_are_candidates_each() {
    let tmp = TempDir::new("dispatch");
    let (shim, llvm) = (tmp.subdir("shim"), tmp.subdir("llvm"));
    program(&shim.join("ccache"));
    symlink(shim.join("ccache"), shim.join("clang")).unwrap();
    symlink(shim.join("ccache"), shim.join("clang-29")).unwrap();
    program(&llvm.join("clang-23"));
    symlink(llvm.join("clang-23"), llvm.join("clang")).unwrap();
    let path = std::env::join_paths([&shim, &llvm]).unwrap();
    let list = ranked_candidates(Some(&path), Path::new("/nonexistent/clang"));
    assert_eq!(
        tmp.within(list),
        [
            shim.join("clang"),
            llvm.join("clang"),
            shim.join("clang-29")
        ]
    );
}

/// `clang` first on `PATH` (Apple's shim in `/usr/bin`, say) doesn't hide another further on.
#[cfg(unix)]
#[test]
fn every_match_on_path_is_a_candidate() {
    let tmp = TempDir::new("shadowed");
    let (first, later) = (tmp.subdir("first"), tmp.subdir("later"));
    program(&first.join("clang"));
    program(&later.join("clang"));
    program(&first.join("clang-30"));
    program(&later.join("clang-30"));
    let path = std::env::join_paths([&first, &later]).unwrap();
    let list = ranked_candidates(Some(&path), &first.join("clang"));
    assert_eq!(
        tmp.within(list),
        [
            later.join("clang"),
            first.join("clang-30"),
            later.join("clang-30")
        ]
    );
}

/// As `execvp` would: a file that can't be started doesn't hide one further on `PATH`.
#[cfg(unix)]
#[test]
fn ranked_candidates_are_files_that_can_be_started() {
    let tmp = TempDir::new("no-exec");
    let (one, two) = (tmp.subdir("one"), tmp.subdir("two"));
    std::fs::write(one.join("clang-29"), "").unwrap();
    program(&two.join("clang-29"));
    let path = std::env::join_paths([&one, &two]).unwrap();
    let list = ranked_candidates(Some(&path), Path::new("/nonexistent/clang"));
    assert_eq!(tmp.within(list), [two.join("clang-29")]);
}

/// The fixed order asks, and records, the file the name would start, not the first of that name:
/// asking `b/clang` and then compiling with `a/clang` fails with `Permission denied`.
#[cfg(unix)]
#[test]
fn the_fixed_order_asks_the_file_that_would_start() {
    let tmp = TempDir::new("fixed");
    let (a, b) = (tmp.subdir("a"), tmp.subdir("b"));
    std::fs::write(a.join("clang"), "").unwrap();
    program(&b.join("clang"));
    let path = std::env::join_paths([&a, &b]).unwrap();
    let want = b.join("clang");
    let probe = |clang: &Path| {
        assert_eq!(clang, want, "asked");
        judge("clang version 19.1.7")
    };
    let search = first_usable(
        vec![PathBuf::from("clang")],
        Why::FirstInOrder,
        Some(&path),
        &all_compile(&probe),
    );
    assert_eq!(search.found.unwrap().path, want);
}

/// `--version` answers on a toolchain for another target only: the one that also compiles wins.
#[test]
fn a_clang_that_cannot_compile_falls_back_to_the_next() {
    let answers = [
        ("wasi/clang-29", Some("clang version 29.0.0")),
        ("b/clang", Some("clang version 19.1.7")),
        (SYSTEM, APPLE),
    ];
    let probe = machine(&answers);
    let asked = RefCell::new(vec![]);
    let compiles = |clang: &Path, _: &str| {
        asked.borrow_mut().push(clang.to_str().unwrap().to_string());
        match clang == Path::new("wasi/clang-29") {
            true => Err("no available targets".to_string()),
            false => Ok(()),
        }
    };
    let ask = Ask {
        version: &probe,
        compiles: &compiles,
    };
    let list = paths(&answers[..2]);
    let search = newest(list, Some(SYSTEM.into()), &ask);
    assert_eq!(chosen(&search), "b/clang");
    assert_eq!(search.why, Why::Newest);
    assert_eq!(
        *asked.borrow(),
        ["wasi/clang-29", "b/clang"],
        "only until one compiles, and not the system clang"
    );
    let text = search.describe().unwrap();
    assert!(
        text.contains("wasi/clang-29 (clang 29.0.0) can't compile for this machine: no available"),
        "{text}"
    );
    let search = first_usable(paths(&answers), Why::FirstInOrder, None, &ask);
    assert_eq!(chosen(&search), "b/clang");
}

/// With the only clang from LLVM unable to compile, the system clang is asked after all.
#[test]
fn the_system_clang_is_the_fallback_from_one_that_cannot_compile() {
    let answers = [
        ("wasi/clang-29", Some("clang version 29.0.0")),
        (SYSTEM, APPLE),
    ];
    let probe = machine(&answers);
    let ask = Ask {
        version: &probe,
        compiles: &|clang, _| match clang == Path::new("wasi/clang-29") {
            true => Err("no available targets".to_string()),
            false => Ok(()),
        },
    };
    let search = newest(paths(&answers[..1]), Some(SYSTEM.into()), &ask);
    assert_eq!(chosen(&search), SYSTEM);
    assert_eq!(search.why, Why::OnlySystem);
}

/// With none that compiles here the first is kept: it may be a cross compiler for the target the
/// build names, and if it isn't the build says what is wrong with it.
#[test]
fn with_none_that_compiles_the_first_is_kept() {
    let answers = [
        ("x/clang-29", Some("clang version 29.0.0")),
        (SYSTEM, APPLE),
    ];
    let probe = machine(&answers);
    let ask = Ask {
        version: &probe,
        compiles: &|_, _| Err("no available targets".to_string()),
    };
    let search = newest(paths(&answers[..1]), Some(SYSTEM.into()), &ask);
    assert_eq!(chosen(&search), "x/clang-29");
    assert_eq!(search.why, Why::NoneCompiles);
    let search = first_usable(paths(&answers), Why::FirstInOrder, None, &ask);
    assert_eq!(chosen(&search), "x/clang-29");
    assert_eq!(search.why, Why::NoneCompiles);
    assert!(search
        .describe()
        .unwrap()
        .contains("no clang found compiles"));
}

/// One that hangs is passed over, and `velt doctor` says so; alone, the build's message does.
#[test]
fn a_clang_that_times_out_falls_back_to_the_next() {
    let probe = |clang: &Path| match clang == Path::new("a/clang-30") {
        true => Probe::TimedOut,
        false => judge("clang version 19.1.7"),
    };
    let list = vec![PathBuf::from("a/clang-30"), PathBuf::from("b/clang")];
    let search = newest(list, None, &all_compile(&probe));
    assert_eq!(chosen(&search), "b/clang");
    let text = search.describe().unwrap();
    assert!(
        text.contains("`a/clang-30` did not answer `--version` within"),
        "{text}"
    );
    let hangs = |_: &Path| Probe::TimedOut;
    let list = vec![PathBuf::from("a/clang-30")];
    let search = first_usable(list, Why::Explicit, None, &all_compile(&hangs));
    assert!(search.found.is_none());
    assert!(search.rejected.unwrap().contains("did not answer"));
}

/// A directory for a test's files, removed when it ends.
#[cfg(unix)]
struct TempDir(PathBuf);

#[cfg(unix)]
impl TempDir {
    fn new(name: &str) -> TempDir {
        let dir = std::env::temp_dir().join(format!("velt-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir.canonicalize().unwrap())
    }

    fn subdir(&self, name: &str) -> PathBuf {
        let dir = self.0.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Those of `list` in this directory: the rest is this machine's.
    fn within(&self, list: Vec<PathBuf>) -> Vec<PathBuf> {
        list.into_iter()
            .filter(|c| c.starts_with(&self.0))
            .collect()
    }
}

#[cfg(unix)]
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// An empty file that may be started.
#[cfg(unix)]
fn program(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, "").unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}
