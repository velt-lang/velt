//! Whether a program can reach the outside world (#1855 step 3): the runtime functions it can
//! call from its entry ([`velt_opt::used_externs`]) that read or change something besides the
//! program's own memory. A test harness is also the test runner (`std/test.vlt` `__run`, which
//! starts processes and reads files and the clock), so the runner is left out: what counts is
//! what the tests reach, through the functions the harness registers. `velt test` keeps a test's result only
//! for a program that reaches none: such a test passes again as long as its program is the same.

use velt_vir::vir::Program;

/// The outside world, by the prefix of the runtime functions that reach it.
const OUTSIDE: &[(&str, &str)] = &[
    ("velt_rt_fs_", "the file system"),
    ("velt_rt_tcp_", "the network"),
    ("velt_rt_udp_", "the network"),
    ("velt_rt_http_", "the network"),
    ("velt_rt_tls_", "the network"),
    ("velt_rt_dns_", "the network"),
    ("velt_rt_child_", "child processes"),
    ("velt_rt_env_", "the environment"),
    ("velt_rt_process_", "the environment"),
    ("velt_rt_os_", "the environment"),
    ("velt_rt_date_now", "the clock"),
    ("velt_rt_perf_now", "the clock"),
    ("velt_rt_signal_timeout", "the clock"),
    ("velt_rt_random_", "random numbers"),
    ("velt_rt_prng_", "random numbers"),
    ("velt_rt_stdin_", "standard input"),
];

/// What `program` reaches outside itself, as the build stamp records it: `-` for nothing, else a
/// message naming it (`the clock, the network`). For a test harness, one entry per test file,
/// `files <file 0>|<file 1>|…`: what each file's tests reach, from the function that registers
/// them (`harness_source`'s `__velt_r<i>`), the other files' left out. A release build that
/// inlined those functions gives every file what the whole harness reaches.
pub(crate) fn reaches(program: &Program) -> String {
    let files = harness_files(program);
    if files == 0 {
        return describe(program, &|symbol| is_test_runner(symbol));
    }
    let each: Vec<String> = (0..files)
        .map(|i| {
            describe(program, &|symbol: &str| {
                is_test_runner(symbol) || harness_file(symbol).is_some_and(|j| j != i)
            })
        })
        .collect();
    format!("files {}", each.join("|"))
}

/// What `program` reaches from its entry, not following the functions `skip` accepts: `-` or
/// the reasons, sorted.
fn describe(program: &Program, skip: &dyn Fn(&str) -> bool) -> String {
    let mut reasons: Vec<String> = velt_opt::used_externs(program, skip)
        .into_iter()
        .filter_map(|id| reason(&program.ext(id).symbol))
        .collect();
    reasons.sort();
    reasons.dedup();
    match reasons.is_empty() {
        true => "-".to_string(),
        false => reasons.join(", "),
    }
}

/// How many test files a harness program registers (its `__velt_r<i>` functions); 0 for a
/// program that is no harness.
fn harness_files(program: &Program) -> usize {
    (program.funcs.iter())
        .filter_map(|f| harness_file(&f.symbol))
        .map(|i| i + 1)
        .max()
        .unwrap_or(0)
}

/// The test file whose tests the function `symbol` registers: `__velt_r<i>` of a harness, its
/// underscores doubled by the mangling (`N12____velt__r0`).
fn harness_file(symbol: &str) -> Option<usize> {
    let at = symbol.find("____velt__r")?;
    let digits: String = symbol[at + "____velt__r".len()..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    let rest = &symbol[at + "____velt__r".len() + digits.len()..];
    // The whole name, not a longer one that starts the same.
    (rest.is_empty() || rest.starts_with('$')).then_some(())?;
    digits.parse().ok()
}

/// What file `index` of a stamp's `outside` record reaches (`None`: nothing): the program's
/// record for a program that is no harness, else that file's entry.
pub(crate) fn of_file(record: &str, index: usize) -> Option<String> {
    let entry = match record.strip_prefix("files ") {
        Some(files) => files.split('|').nth(index).unwrap_or("-"),
        None => record,
    };
    (entry != "-").then(|| entry.to_string())
}

/// Whether `symbol` is `velt:test`'s `__run` (its state machine's `$poll` and `$drop` too).
fn is_test_runner(symbol: &str) -> bool {
    symbol.contains("V3stdP4testN7____run")
}

/// What calling the runtime function `symbol` reaches, if anything. A native package's function
/// (`velt_<package>__…`) may do anything.
fn reason(symbol: &str) -> Option<String> {
    if let Some((_, what)) = OUTSIDE
        .iter()
        .find(|(prefix, _)| symbol.starts_with(prefix))
    {
        return Some(what.to_string());
    }
    // Velt's own: the runtime, and the names packages can't take (`rt`, `sig`, `native`).
    let own = [
        "velt_rt_",
        "velt_atomic_",
        "velt_simd",
        "velt_sig",
        "velt_native",
    ];
    if own.iter().any(|p| symbol.starts_with(p)) {
        return None;
    }
    let package = symbol.strip_prefix("velt_")?.split_once("__")?.0;
    Some(format!("native code of `{package}`"))
}

#[cfg(test)]
mod tests {
    use super::reason;

    #[test]
    fn harness_files_are_found_by_their_registering_function() {
        use super::{harness_file, of_file};
        assert_eq!(harness_file("_V0M0M0P2hN12____velt__r0"), Some(0));
        assert_eq!(harness_file("_V0M0M0P2hN13____velt__r12$poll"), Some(12));
        assert_eq!(harness_file("_V0M0M0P2hN14____velt__r1x"), None);
        assert_eq!(
            of_file("files -|the clock", 1).as_deref(),
            Some("the clock")
        );
        assert_eq!(of_file("files -|the clock", 0), None);
        assert_eq!(of_file("the network", 3).as_deref(), Some("the network"));
        assert_eq!(of_file("-", 0), None);
    }

    #[test]
    fn runtime_functions_are_sorted_into_what_they_reach() {
        assert_eq!(
            reason("velt_rt_fs_read_file_sync").as_deref(),
            Some("the file system")
        );
        assert_eq!(
            reason("velt_rt_tcp_connect").as_deref(),
            Some("the network")
        );
        assert_eq!(reason("velt_rt_perf_now").as_deref(), Some("the clock"));
        assert_eq!(
            reason("velt_rt_prng_f64").as_deref(),
            Some("random numbers")
        );
        assert_eq!(
            reason("velt_sqlite__open").as_deref(),
            Some("native code of `sqlite`")
        );
        // The program's own memory, strings, timers and threads stay inside.
        for inside in [
            "velt_rt_str_concat",
            "velt_rt_timer_new",
            "velt_rt_json_parse",
            "velt_rt_alloc",
            "velt_atomic_load_u64",
            "velt_rt_any__get",
            "memcpy",
        ] {
            assert_eq!(reason(inside), None, "{inside}");
        }
    }
}
