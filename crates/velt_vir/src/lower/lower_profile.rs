//! Where the lowering spends its time: with `VELT_LOWER_PROFILE` set, each lowering pass prints
//! its time and function count, and the slowest functions by their own time (nested builds
//! excluded) with their size, to stderr. Off, a function build costs a thread-local access and
//! a `RefCell` borrow.

use std::cell::RefCell;
use std::time::{Duration, Instant};

use crate::vir;

#[derive(Default)]
struct Prof {
    /// Per function being built: the time of the builds nested in it.
    stack: Vec<Duration>,
    /// Per built function: symbol, own time, blocks, statements.
    fns: Vec<(String, Duration, usize, usize)>,
}

thread_local! {
    static PROF: RefCell<Option<Prof>> = const { RefCell::new(None) };
}

/// Start a lowering pass.
pub(super) fn start_pass() {
    let on = std::env::var_os("VELT_LOWER_PROFILE").is_some();
    PROF.with(|p| *p.borrow_mut() = on.then(Prof::default));
}

/// A function build starts; `None` when profiling is off.
pub(super) fn enter() -> Option<Instant> {
    PROF.with(|p| {
        let mut p = p.borrow_mut();
        let p = p.as_mut()?;
        p.stack.push(Duration::ZERO);
        Some(Instant::now())
    })
}

/// The build started at `t` produced `f`.
pub(super) fn leave(t: Option<Instant>, f: &vir::Function) {
    let Some(t) = t else { return };
    let total = t.elapsed();
    PROF.with(|p| {
        let mut p = p.borrow_mut();
        let Some(p) = p.as_mut() else { return };
        let nested = p.stack.pop().unwrap_or_default();
        if let Some(outer) = p.stack.last_mut() {
            *outer += total;
        }
        let stmts = f.blocks.iter().map(|b| b.stmts.len()).sum();
        p.fns.push((
            f.symbol.clone(),
            total.saturating_sub(nested),
            f.blocks.len(),
            stmts,
        ));
    });
}

/// End pass `n`, which took `d`: print its summary.
pub(super) fn end_pass(n: usize, d: Duration) {
    let Some(mut p) = PROF.with(|p| p.borrow_mut().take()) else {
        return;
    };
    let blocks: usize = p.fns.iter().map(|f| f.2).sum();
    let stmts: usize = p.fns.iter().map(|f| f.3).sum();
    let mut out = format!(
        "velt-lower: pass {n}: {:.1} ms, {} functions, {blocks} blocks, {stmts} statements\n",
        d.as_secs_f64() * 1e3,
        p.fns.len()
    );
    p.fns.sort_by_key(|f| std::cmp::Reverse(f.1));
    for (sym, own, b, s) in p.fns.iter().take(25) {
        // Instance symbols spell their type arguments out, some in tens of kilobytes.
        let sym: String = sym.chars().take(160).collect();
        out.push_str(&format!(
            "velt-lower:   {:>10.1} ms {b:>7} blocks {s:>8} stmts  {sym}\n",
            own.as_secs_f64() * 1e3
        ));
    }
    eprint!("{out}");
}
