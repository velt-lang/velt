//! Whether a program can reach the outside world (#1855 step 3): the runtime functions it can
//! call from its entry ([`velt_opt::used_externs`]) that read or change something besides the
//! program's own memory. A test harness is also the test runner (`std/test/run.vlt` `__run`, which
//! starts processes and reads files and the clock), so the runner is left out: what counts is
//! what the tests reach, through the functions the harness registers. `velt test` keeps a test's result only
//! for a program that reaches none: such a test passes again as long as its program is the same.

use velt_vir::vir::Program;

/// The runtime's function groups (`velt_rt_<group>_…`) that touch nothing outside the program:
/// strings, collections, numbers, JSON and regex, futures, tasks, threads, channels and locks,
/// timers, `any`, proxies, memory management, and output (printing is not input). Any other
/// runtime function counts as outside, so one added later is safe until it is listed here.
const PURE_RUNTIME: &[&str] = &[
    "all",
    "alloc",
    "any",
    "array",
    "await",
    "bigint",
    "block",
    "bytes",
    "cell",
    "chan",
    "check",
    "clear",
    "deep",
    "drop",
    "err",
    "exit",
    "f64",
    "flush",
    "fnprop",
    "fnv1a64",
    "free",
    "fut",
    "futs",
    "global",
    "group",
    "html",
    "i64",
    "json",
    "jsx",
    "keep",
    "latch",
    "lk",
    "math",
    "microtask",
    "module",
    "mutex",
    "panic",
    "param",
    "parse",
    "pow",
    "proxy",
    "race",
    "rc",
    "realloc",
    "regex",
    "rwlock",
    "set",
    "signal",
    "sleep",
    "spawn",
    "stdout",
    "str",
    "strbuf",
    "symbol",
    "task",
    "thread",
    "throw",
    "timer",
    "tla",
    "u64",
    "uncaught",
    "unhandled",
    "view",
    "weak",
    "weakmap",
    "weakref",
    "write",
    "xfer",
    "yield",
];

/// Functions of a pure group that do reach outside, with what they reach.
const IMPURE_IN_PURE_GROUP: &[(&str, &str)] = &[("velt_rt_signal_timeout", "the clock")];

/// What the impure runtime groups reach, for the message (others: `a runtime function …`).
const RUNTIME_REACH: &[(&str, &str)] = &[
    ("fs", "the file system"),
    ("tcp", "the network"),
    ("udp", "the network"),
    ("http", "the network"),
    ("tls", "the network"),
    ("dns", "the network"),
    ("child", "child processes"),
    ("env", "the environment"),
    ("process", "the environment"),
    ("os", "the environment"),
    ("isatty", "the environment"),
    ("memory", "the environment"),
    ("worker", "the environment"),
    ("current", "the environment"),
    ("date", "the clock"),
    ("perf", "the clock"),
    ("local", "the clock"),
    ("random", "random numbers"),
    ("prng", "random numbers"),
    ("stdin", "standard input"),
    ("native", NATIVE_SUPPORT),
];

/// What the runtime's support for native packages (`velt_rt_native_*`) reaches.
const NATIVE_SUPPORT: &str = "native code";

/// Velt's own functions besides the runtime's that touch nothing outside: SIMD, atomics,
/// thread-local storage and CPU feature checks (with the underscore, and no `__`: a package's
/// exports are `velt_<package>__…`, and a package may be called `simdjson` or `atomic_ops`).
const PURE_OWN: &[&str] = &["velt_simd_", "velt_atomic_", "velt_tls_", "velt_cpu_"];

/// What a record entry stands for when the build can't attribute code to a test file (its
/// registering function was inlined) or has no entry for it.
pub(crate) const UNKNOWN: &str = "more than the build can tell (its registration was inlined)";

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
            let present = program
                .funcs
                .iter()
                .any(|f| harness_file(&f.symbol) == Some(i));
            match present {
                // Inlined into `main`: what it reaches is mixed with the others'.
                false => "?".to_string(),
                true => describe(program, &|symbol: &str| {
                    is_test_runner(symbol) || harness_file(symbol).is_some_and(|j| j != i)
                }),
            }
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
    // The runtime's support for native packages (`velt_rt_native_*`) comes with a package's
    // code, which names what it is.
    if reasons.iter().any(|r| r.starts_with("native code of")) {
        reasons.retain(|r| r != NATIVE_SUPPORT);
    }
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

/// The test file whose tests the function `symbol` registers: `__velt_r<i>` of the harness, the
/// program's entry module, whose functions have no module path (`_V12____velt__r0`, underscores
/// doubled by the mangling). A function of that name in a test file or anything it imports has
/// its module in front (`_V5testsP6s_2etestN12____velt__r7`) and is that file's own code.
fn harness_file(symbol: &str) -> Option<usize> {
    let rest = symbol.strip_prefix("_V")?;
    let len_digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    let len: usize = rest[..len_digits].parse().ok()?;
    let rest = &rest[len_digits..];
    let name = rest.get(..len)?;
    let index = name.strip_prefix("____velt__r")?;
    let after = &rest[len..];
    // The whole symbol: the name itself, or its poll function.
    (after.is_empty() || after.starts_with('$')).then_some(())?;
    (!index.is_empty() && index.bytes().all(|b| b.is_ascii_digit())).then_some(())?;
    index.parse().ok()
}

/// What file `index` of a stamp's `outside` record reaches (`None`: nothing): the program's
/// record for a program that is no harness, else that file's entry. A file without an entry, or
/// whose code the build couldn't attribute (`?`), reaches [`UNKNOWN`]: never nothing.
pub(crate) fn of_file(record: &str, index: usize) -> Option<String> {
    let entry = match record.strip_prefix("files ") {
        Some(files) => files.split('|').nth(index).unwrap_or("?"),
        None => record,
    };
    match entry {
        "-" => None,
        "?" | "" => Some(UNKNOWN.to_string()),
        reason => Some(reason.to_string()),
    }
}

/// Whether `symbol` is `velt/test`'s `__run` (its state machine's `$poll` and `$drop` too).
fn is_test_runner(symbol: &str) -> bool {
    symbol.contains("V3stdP4testP3runN7____run")
}

/// What calling the extern `symbol` reaches, if anything. Only what is known to be pure is
/// inside; a system function (`open`, `CreateFileW`), a native package's function and anything
/// else unknown is outside.
fn reason(symbol: &str) -> Option<String> {
    if let Some((_, what)) = IMPURE_IN_PURE_GROUP
        .iter()
        .find(|(p, _)| symbol.starts_with(p))
    {
        return Some(what.to_string());
    }
    let Some(rest) = symbol.strip_prefix("velt_") else {
        return Some(format!("a system function `{symbol}`"));
    };
    if let Some(runtime) = rest.strip_prefix("rt_") {
        let group = runtime.split('_').next().unwrap_or("");
        if PURE_RUNTIME.contains(&group) {
            return None;
        }
        return Some(match RUNTIME_REACH.iter().find(|(g, _)| *g == group) {
            Some((_, what)) => what.to_string(),
            None => format!("a runtime function `{symbol}`"),
        });
    }
    if let Some((package, _)) = rest.split_once("__") {
        return Some(format!("native code of `{package}`"));
    }
    if let Some(package) = rest.strip_prefix("native_init_") {
        return Some(format!("native code of `{package}`"));
    }
    if PURE_OWN.iter().any(|p| symbol.starts_with(p)) {
        return None;
    }
    Some(format!("a runtime function `{symbol}`"))
}

#[cfg(test)]
mod tests {
    use super::{harness_file, of_file, reaches, reason, UNKNOWN};
    use velt_vir::vir::{
        BasicBlock, BlockId, Callee, Const, ExternFn, ExternId, FuncId, Function, Linkage, Operand,
        Program, Terminator, Ty,
    };

    /// A function `symbol` that calls `callees` in turn.
    fn func(symbol: &str, linkage: Linkage, callees: &[Callee]) -> Function {
        let mut blocks: Vec<BasicBlock> = callees
            .iter()
            .enumerate()
            .map(|(i, callee)| BasicBlock {
                stmts: vec![],
                term: Terminator::Call {
                    callee: callee.clone(),
                    args: vec![],
                    dest: None,
                    next: BlockId(i as u32 + 1),
                },
            })
            .collect();
        blocks.push(BasicBlock {
            stmts: vec![],
            term: Terminator::Return(Operand::Const(Const::Unit, Ty::Unit)),
        });
        Function {
            symbol: symbol.into(),
            params: vec![],
            ret: Ty::Unit,
            locals: vec![],
            blocks,
            linkage,
            locs: vec![],
            param_attrs: vec![],
            is_poll: false,
            target: None,
            cold: false,
        }
    }

    /// A harness of three files whose registering function for file 1 was inlined into `main`:
    /// file 0 reads the clock, file 2 nothing, and file 1's code (the file system) sits in `main`.
    #[test]
    fn a_file_whose_registration_was_inlined_is_never_pure() {
        let ext = |symbol: &str| ExternFn {
            symbol: symbol.into(),
            params: vec![],
            ret: Ty::Unit,
            noreturn: false,
        };
        let r0 = func(
            "_V12____velt__r0",
            Linkage::Internal,
            &[Callee::Extern(ExternId(0))],
        );
        let r2 = func("_V12____velt__r2", Linkage::Internal, &[]);
        let main = func(
            "velt_main",
            Linkage::Export,
            &[
                Callee::Func(FuncId(0)),
                Callee::Extern(ExternId(1)),
                Callee::Func(FuncId(1)),
            ],
        );
        let program = Program {
            aggs: vec![],
            funcs: vec![r0, r2, main],
            externs: vec![ext("velt_rt_date_now"), ext("velt_rt_fs_stat_sync")],
            statics: vec![],
            files: vec![],
            debug_types: vec![],
        };
        let record = reaches(&program);
        // `main`'s own code counts for every file, the inlined file 1 is unknown.
        assert_eq!(record, "files the clock, the file system|?|the file system");
        assert_eq!(of_file(&record, 1).as_deref(), Some(UNKNOWN));
        // A fourth file the record has no entry for is unknown too, never pure.
        assert_eq!(of_file(&record, 3).as_deref(), Some(UNKNOWN));
    }

    #[test]
    fn harness_files_are_found_by_their_registering_function() {
        assert_eq!(harness_file("_V12____velt__r0"), Some(0));
        assert_eq!(harness_file("_V13____velt__r12$poll"), Some(12));
        assert_eq!(harness_file("_V13____velt__r1x"), None);
        assert_eq!(harness_file("_V12____velt__r0_T3i64"), None);
        // A user's function of the same name, in a test file's module, is that file's code.
        assert_eq!(harness_file("_V5testsP6s_2etestN12____velt__r7"), None);
        assert_eq!(harness_file("_V5testsN12____velt__r7"), None);
        assert_eq!(
            of_file("files -|the clock", 1).as_deref(),
            Some("the clock")
        );
        assert_eq!(of_file("files -|the clock", 0), None);
        assert_eq!(of_file("the network", 3).as_deref(), Some("the network"));
        assert_eq!(of_file("-", 0), None);
        // A file the record has no entry for, or whose code was inlined, is never pure.
        assert_eq!(of_file("files the clock", 1).as_deref(), Some(UNKNOWN));
        assert_eq!(of_file("files ?|-", 0).as_deref(), Some(UNKNOWN));
    }

    #[test]
    fn only_what_is_known_to_be_pure_is_inside() {
        let outside = |s: &str| reason(s).unwrap_or_else(|| panic!("{s} counted as inside"));
        assert_eq!(outside("velt_rt_fs_read_file_sync"), "the file system");
        assert_eq!(outside("velt_rt_tcp_connect"), "the network");
        assert_eq!(outside("velt_rt_perf_now"), "the clock");
        assert_eq!(outside("velt_rt_local_offset_minutes"), "the clock");
        assert_eq!(outside("velt_rt_signal_timeout_ms"), "the clock");
        assert_eq!(outside("velt_rt_prng_f64"), "random numbers");
        assert_eq!(outside("velt_rt_memory_rss"), "the environment");
        assert_eq!(outside("velt_rt_isatty"), "the environment");
        assert_eq!(outside("velt_rt_worker_count"), "the environment");
        // System functions, native packages (also those named like Velt's own prefixes), and
        // runtime functions not listed as pure.
        assert_eq!(outside("open"), "a system function `open`");
        assert_eq!(outside("velt_sqlite__open"), "native code of `sqlite`");
        assert_eq!(outside("velt_sigx__get"), "native code of `sigx`");
        assert_eq!(outside("velt_simdjson__parse"), "native code of `simdjson`");
        assert_eq!(
            outside("velt_atomic_ops__cas"),
            "native code of `atomic_ops`"
        );
        assert_eq!(outside("velt_native_init_sigx"), "native code of `sigx`");
        assert_eq!(outside("velt_rt_native_api"), "native code");
        assert_eq!(
            outside("velt_rt_newthing_x"),
            "a runtime function `velt_rt_newthing_x`"
        );
        assert_eq!(
            outside("velt_std_sqlite_open"),
            "a runtime function `velt_std_sqlite_open`"
        );
        for inside in [
            "velt_rt_str_concat",
            "velt_rt_timer_new",
            "velt_rt_json_parse",
            "velt_rt_alloc",
            "velt_rt_signal_new",
            "velt_rt_uncaught_now",
            "velt_atomic_load_u64",
            "velt_simd_add_f32x4",
        ] {
            assert_eq!(reason(inside), None, "{inside}");
        }
    }
}
