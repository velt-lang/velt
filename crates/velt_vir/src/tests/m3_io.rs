//! std/fs and std/net shapes over `declare async function` rt externs (emulated by the test
//! interpreter): wrappers that await the extern's `IoResult` and throw `IoError`, `try`/`catch`
//! and `try`/`finally` around awaits, errors propagating through awaited async calls, the
//! `tcp_echo` golden (a spawned server task waiting on accept/read while main connects), and a
//! throwing `async main`.

use velt_sema::hir::{Intrinsic as I, Program, UseMode as U};

use super::builder::*;
use super::builder_m2::*;
use super::builder_m3::*;
use super::programs_m3::{io_types, tcp_echo, wrapper};
use super::{m3_golden, run};

fn fs_program(finally_test: bool) -> Program {
    let mut pb = PB::new();
    let t = pb.t;
    let io = io_types(&mut pb);
    let read = wrapper(
        &mut pb,
        &io,
        "readFile",
        "velt_rt_fs_read_file",
        &[("path", t.str)],
        Some(t.str),
    );
    let write = wrapper(
        &mut pb,
        &io,
        "writeFile",
        "velt_rt_fs_write_file",
        &[("path", t.str), ("data", t.str)],
        None,
    );
    let (ps, pv) = (pb.promise(t.str), pb.promise(t.unit));
    // `async function g() { try { await readFile("missing.txt"); } finally { log("finally g") } }`
    let g = {
        let mut f = FB::new("g", pv);
        f.throws = Some(io.err);
        let body = vec![try_(
            vec![se(await_(call(read, vec![s("missing.txt", t)], ps), t.str))],
            None,
            Some(vec![se(print(vec![s("finally g", t)], t))]),
        )];
        pb.add_fn(f.build_async(body))
    };
    let mut f = FB::new("main", pv);
    f.throws = Some(io.err);
    let a = f.local("a", t.str);
    let e = f.local("e", io.err);
    let e2 = f.local("e2", io.err);
    let code = |f: &FB, e| field(f.bw(e), 0, U::Borrow, t.str);
    let mut body = vec![
        se(await_(
            call(write, vec![s("a.txt", t), s("hello", t)], pv),
            t.unit,
        )),
        let_(a, await_(call(read, vec![s("a.txt", t)], ps), t.str)),
        se(print(
            vec![f.bw(a), intr(I::StrLen, vec![f.bw(a)], t.usize)],
            t,
        )),
        try_(
            vec![se(await_(call(read, vec![s("missing.txt", t)], ps), t.str))],
            Some((
                Some(e),
                vec![se(print(vec![s("error:", t), code(&f, e)], t))],
            )),
            None,
        ),
    ];
    if finally_test {
        body.push(try_(
            vec![se(await_(call(g, vec![], pv), t.unit))],
            Some((
                Some(e2),
                vec![se(print(vec![s("caught", t), code(&f, e2)], t))],
            )),
            None,
        ));
    }
    pb.add_main(f.build_async(body));
    pb.finish()
}

#[test]
fn fs_wrappers_and_catch() {
    let out = run(&fs_program(false));
    assert_eq!(out.stdout, "hello 5\nerror: ENOENT\n");
}

#[test]
fn finally_around_await_and_propagation() {
    let out = run(&fs_program(true));
    assert_eq!(
        out.stdout,
        "hello 5\nerror: ENOENT\nfinally g\ncaught ENOENT\n"
    );
}

/// Uncaught error from a throwing `async main`: reported like a sync main, exit 1.
#[test]
fn async_main_uncaught() {
    let mut pb = PB::new();
    let t = pb.t;
    let io = io_types(&mut pb);
    let read = wrapper(
        &mut pb,
        &io,
        "readFile",
        "velt_rt_fs_read_file",
        &[("path", t.str)],
        Some(t.str),
    );
    let (ps, pv) = (pb.promise(t.str), pb.promise(t.unit));
    let mut f = FB::new("main", pv);
    f.throws = Some(io.err);
    let body = vec![
        se(print(vec![s("before", t)], t)),
        se(await_(call(read, vec![s("nope", t)], ps), t.str)),
        se(print(vec![s("after", t)], t)),
    ];
    pb.add_main(f.build_async(body));
    let out = run(&pb.finish());
    assert_eq!(out.stdout, "before\n");
    assert_eq!(out.stderr, "Uncaught IoError: no such file: nope\n");
    assert_eq!(out.code, 1);
}

/// Promise *values* of a throwing async fn (stored in an array for `Promise.all`, spawned)
/// have type `Promise<T, E>`: awaiting one rethrows its error (here uncaught in `main`).
fn stored_throwing_promises(fail: bool) -> Program {
    let mut pb = PB::new();
    let t = pb.t;
    let io = io_types(&mut pb);
    let read = wrapper(
        &mut pb,
        &io,
        "readFile",
        "velt_rt_fs_read_file",
        &[("path", t.str)],
        Some(t.str),
    );
    let write = wrapper(
        &mut pb,
        &io,
        "writeFile",
        "velt_rt_fs_write_file",
        &[("path", t.str), ("data", t.str)],
        None,
    );
    let pv = pb.promise(t.unit);
    let ps = pb.ty(velt_sema::hir::TyKind::Promise(t.str, io.err));
    let (aps, sa) = (pb.arr(ps), pb.arr(t.str));
    let psa = pb.ty(velt_sema::hir::TyKind::Promise(sa, io.err));
    let mut f = FB::new("main", pv);
    f.throws = Some(io.err);
    let rs = f.local("rs", sa);
    let h = f.local("h", ps);
    let second = if fail { "missing.txt" } else { "b.txt" };
    let body = vec![
        se(await_(
            call(write, vec![s("a.txt", t), s("A", t)], pv),
            t.unit,
        )),
        se(await_(
            call(write, vec![s("b.txt", t), s("BB", t)], pv),
            t.unit,
        )),
        let_(
            rs,
            await_(
                promise_all(
                    array(
                        vec![
                            call(read, vec![s("a.txt", t)], ps),
                            call(read, vec![s("b.txt", t)], ps),
                        ],
                        aps,
                    ),
                    psa,
                ),
                sa,
            ),
        ),
        se(print(vec![f.bw(rs)], t)),
        let_(h, spawn(call(read, vec![s(second, t)], ps), ps)),
        se(print(vec![await_(f.mv(h), t.str)], t)),
    ];
    pb.add_main(f.build_async(body));
    pb.finish()
}

#[test]
fn stored_promises_of_throwing_fns() {
    let out = run(&stored_throwing_promises(false));
    assert_eq!(out.stdout, "[ 'A', 'BB' ]\nBB\n");
    let out = run(&stored_throwing_promises(true));
    assert_eq!(out.stdout, "[ 'A', 'BB' ]\n");
    assert_eq!(out.stderr, "Uncaught IoError: no such file: missing.txt\n");
    assert_eq!(out.code, 1);
}

#[test]
fn tcp_echo_golden() {
    let out = run(&tcp_echo());
    assert_eq!(out.stdout, m3_golden("tcp_echo"));
    assert_eq!(out.code, 0);
}
