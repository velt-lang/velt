//! `__intrinsic_http_handler` on the http_server golden's shape (programs_http.rs), run by the
//! interpreter against an emulated server with scripted requests: every request gets its own
//! state from `init`, both async closures run once per request (their captures are borrowed
//! from the shared environment or cloned per call, never moved out), and nothing leaks.

use super::interp::run_http;
use super::lower_ok;
use super::programs_http::http_server;
use crate::vir::Terminator;

#[test]
fn handler_serves_every_request() {
    let v = lower_ok(&http_server());
    let requests = [
        ("GET", "/a", ""),
        ("POST", "/echo", "hi"),
        ("GET", "/missing", ""),
    ];
    let (out, responses) = run_http(&v, &requests);
    assert_eq!(out.stdout, "listening 8080\nhits 3\n");
    assert_eq!(out.stderr, "");
    let expected = vec![
        (200, "GET http://localhost/a #1".to_string()),
        (200, "POST http://localhost/echo #2".to_string()),
        (404, "GET http://localhost/missing #3".to_string()),
    ];
    assert_eq!(responses, expected);
    assert_eq!(out.live_allocs, 0, "leaked heap allocations\n{v}");
}

#[test]
fn handler_init_borrows_captures_without_calls() {
    // `init` runs per request on the runtime's workers: it only stores the request pointer and
    // a pointer to the captured handler (no clone, no allocation).
    let v = lower_ok(&http_server());
    let init = v
        .funcs
        .iter()
        .find(|f| f.symbol.ends_with("$init") && !f.symbol.ends_with("$copy$init"))
        .expect("handler init function");
    assert_eq!(init.params.len(), 3);
    let calls = init
        .blocks
        .iter()
        .filter(|b| matches!(b.term, Terminator::Call { .. }))
        .count();
    assert_eq!(calls, 0, "{v}");
    let tuple = v
        .aggs
        .iter()
        .find(|a| a.name == "tuple")
        .expect("handler tuple");
    assert_eq!((tuple.size, tuple.fields.len()), (48, 6));
}

/// The extern and function calls `f` makes, by symbol.
fn calls(v: &crate::vir::Program, f: &crate::vir::Function) -> Vec<String> {
    use crate::vir::Callee;
    f.blocks
        .iter()
        .filter_map(|b| match &b.term {
            Terminator::Call { callee, .. } => Some(match callee {
                Callee::Extern(id) => v.externs[id.0 as usize].symbol.clone(),
                Callee::Func(id) => v.funcs[id.0 as usize].symbol.clone(),
                Callee::Ptr { .. } => "<ptr>".into(),
            }),
            _ => None,
        })
        .collect()
}

#[test]
fn handler_capturing_a_function_value_may_copy_it_per_request() {
    // The adapter captures the user's handler, a function value: `serve` notes whether it
    // reaches captured variables' cells, and if so picks the `init` that copies it for each
    // request, taking turns (#873). The plain `init` stays free of calls.
    let v = lower_ok(&http_server());
    let copying = v
        .funcs
        .iter()
        .find(|f| f.symbol.ends_with("$copy$init"))
        .expect("copying handler init");
    let c = calls(&v, copying);
    for rt in [
        "velt_rt_copy_lock",
        "velt_rt_xfer_begin",
        "velt_rt_xfer_end",
        "velt_rt_copy_unlock",
    ] {
        assert!(c.iter().any(|s| s == rt), "{rt} missing in {c:?}");
    }
    let main = v
        .funcs
        .iter()
        .find(|f| calls(&v, f).iter().any(|s| s == "velt_rt_http_serve"))
        .expect("the function that serves");
    assert!(
        calls(&v, main).iter().any(|s| s == "velt_rt_take_cells"),
        "{v}"
    );
}
