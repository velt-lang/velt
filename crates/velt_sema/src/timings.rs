//! Where the checker spends its time: with `VELT_SEMA_TIMINGS` set, each check prints the time
//! of every pass and of every round of the whole program (`crate::check_on_current_thread`) to
//! stderr. Off, a pass costs one thread-local read.

use std::cell::RefCell;
use std::time::{Duration, Instant};

thread_local! {
    /// The passes timed in this check, in order, with their time; `None` when timing is off.
    static PASSES: RefCell<Option<Vec<(&'static str, Duration)>>> = const { RefCell::new(None) };
}

/// Start timing the passes of a check on this thread, when `VELT_SEMA_TIMINGS` is set.
pub(crate) fn start() {
    let on = std::env::var_os("VELT_SEMA_TIMINGS").is_some();
    PASSES.with(|p| *p.borrow_mut() = on.then(Vec::new));
}

/// Run `f`, the pass `name`, timing it when timing is on.
pub(crate) fn pass<T>(name: &'static str, f: impl FnOnce() -> T) -> T {
    if PASSES.with(|p| p.borrow().is_none()) {
        return f();
    }
    let t = Instant::now();
    let r = f();
    record(name, t.elapsed());
    r
}

/// Record `d` as the time of the pass `name`, when timing is on.
pub(crate) fn record(name: &'static str, d: Duration) {
    PASSES.with(|p| {
        if let Some(v) = p.borrow_mut().as_mut() {
            v.push((name, d));
        }
    });
}

/// Print the passes timed since [`start`]: those that took at least a millisecond, in order,
/// then the total of each pass over all rounds, slowest first.
pub(crate) fn finish() {
    let Some(passes) = PASSES.with(|p| p.borrow_mut().take()) else {
        return;
    };
    let ms = |d: Duration| d.as_secs_f64() * 1e3;
    let mut out = String::from("velt-sema: passes in order (>= 1 ms)\n");
    for (name, d) in passes.iter().filter(|(_, d)| ms(*d) >= 1.0) {
        out.push_str(&format!("velt-sema:   {name:<32} {:>10.1} ms\n", ms(*d)));
    }
    let mut totals: Vec<(&str, Duration, usize)> = Vec::new();
    for &(name, d) in &passes {
        match totals.iter_mut().find(|(n, ..)| *n == name) {
            Some(t) => {
                t.1 += d;
                t.2 += 1;
            }
            None => totals.push((name, d, 1)),
        }
    }
    totals.sort_by_key(|t| std::cmp::Reverse(t.1));
    out.push_str("velt-sema: totals\n");
    for (name, d, n) in totals.iter().filter(|(_, d, _)| ms(*d) >= 1.0) {
        out.push_str(&format!(
            "velt-sema:   {name:<32} {:>10.1} ms  x{n}\n",
            ms(*d)
        ));
    }
    eprint!("{out}");
}
