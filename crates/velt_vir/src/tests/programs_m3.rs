//! HIR programs shared by the interpreter tests and the native tests (`tests/native.rs`
//! includes this file): the M3 goldens `async_basic`, `tasks` and `tcp_echo` (with std-like
//! wrappers over `declare async function` rt externs).

use velt_sema::hir::{
    AdtKind, BinOp as B, Capture, Def, DefId, ExternFnDef, IntTy, Intrinsic, Intrinsic as I,
    PassMode, Program, TyId, TyKind, UseMode as U,
};

use super::builder::*;
use super::builder_m2::*;
use super::builder_m3::*;
use super::builder_prelude::prelude;

/// `async function delayed(ms, v) { await sleep(ms); return v; }`; returns its DefId.
pub(super) fn delayed(pb: &mut PB) -> velt_sema::hir::DefId {
    let t = pb.t;
    let (pi, pv) = (pb.promise(t.i64), pb.promise(t.unit));
    let mut f = FB::new("delayed", pi);
    let ms = f.param("ms", t.i64, PassMode::Copy);
    let v = f.param("v", t.i64, PassMode::Copy);
    let body = vec![se(await_(sleep(f.cp(ms), pv), t.unit)), ret(Some(f.cp(v)))];
    pb.add_fn(f.build_async(body))
}

pub(super) fn async_basic() -> Program {
    let mut pb = PB::new();
    let t = pb.t;
    let delayed = delayed(&mut pb);
    let pi = pb.promise(t.i64);
    let pv = pb.promise(t.unit);
    let ai = pb.arr(t.i64);
    let av = pb.arr(t.unit);
    let (api, apv) = (pb.arr(pi), pb.arr(pv));
    let (pai, pav) = (pb.promise(ai), pb.promise(av));
    let sum_all = {
        let mut f = FB::new("sumAll", pi);
        let xs = f.param("xs", ai, PassMode::Owned);
        let total = f.local("total", t.i64);
        let x = f.local("x", t.i64);
        let body = vec![
            let_(total, int(0, t.i64)),
            for_of(
                pbind(x, U::Copy, t.i64),
                f.bw(xs),
                vec![se(cassign(
                    B::Add,
                    f.bm(total),
                    await_(call(delayed, vec![int(1, t.i64), f.cp(x)], pi), t.i64),
                    t,
                ))],
            ),
            ret(Some(f.cp(total))),
        ];
        pb.add_fn(f.build_async(body))
    };
    let mut f = FB::new("main", pv);
    let a = f.local("a", t.i64);
    let x = f.local("x", t.i64);
    let y = f.local("y", t.i64);
    let start = f.local("start", t.f64);
    let elapsed = f.local("elapsed", t.f64);
    let h = f.local("h", pi);
    let d = |ms, v| call(delayed, vec![int(ms, t.i64), int(v, t.i64)], pi);
    let now = || intr(Intrinsic::PerfNow, vec![], t.f64);
    let body = vec![
        let_(a, await_(d(10, 1), t.i64)),
        se(print(vec![s("a", t), f.cp(a)], t)),
        let_pat(
            pat(
                velt_sema::hir::PatKind::Array {
                    elems: vec![pbind(x, U::Copy, t.i64), pbind(y, U::Copy, t.i64)],
                    rest: None,
                },
                ai,
            ),
            await_(promise_all(array(vec![d(30, 2), d(10, 3)], api), pai), ai),
        ),
        se(print(vec![f.cp(x), f.cp(y)], t)),
        let_(start, now()),
        se(await_(
            promise_all(
                array((0..3).map(|_| sleep(int(50, t.i64), pv)).collect(), apv),
                pav,
            ),
            av,
        )),
        let_(elapsed, bin(B::Sub, now(), f.cp(start))),
        se(print(
            vec![
                cmp(B::GtEq, f.cp(elapsed), flt(45.0, t.f64), t),
                cmp(B::Lt, f.cp(elapsed), flt(140.0, t.f64), t),
            ],
            t,
        )),
        let_(h, spawn(d(5, 42), pi)),
        se(print(vec![await_(f.mv(h), t.i64)], t)),
        se(print(
            vec![await_(
                call(
                    sum_all,
                    vec![array((1..=4).map(|v| int(v, t.i64)).collect(), ai)],
                    pi,
                ),
                t.i64,
            )],
            t,
        )),
    ];
    pb.add_main(f.build_async(body));
    pb.finish()
}

/// `struct Mutex<T> { lock: u64; value: T }` as the prelude declares it.
pub(super) fn mutex_def(pb: &mut PB) -> DefId {
    let u64t = pb.ty(TyKind::Int(velt_sema::hir::IntTy::U64));
    let p0 = pb.param(0);
    let mut m = adt(
        "Mutex",
        AdtKind::Struct,
        vec![("lock", u64t, None), ("value", p0, None)],
    );
    m.generics = 1;
    pb.add_def(Def::Adt(m))
}

/// `for (let i = 0; i < n; i++) body` as sema desugars it.
pub(super) fn count_to(
    f: &FB,
    i: velt_sema::hir::LocalId,
    n: u128,
    t: T,
    body: Vec<velt_sema::hir::Stmt>,
) -> velt_sema::hir::Stmt {
    sblock(vec![
        let_(i, int(0, t.i64)),
        while_(
            None,
            cmp(B::Lt, f.cp(i), int(n, t.i64), t),
            body,
            Some(cassign(B::Add, f.bm(i), int(1, t.i64), t)),
        ),
    ])
}

fn worker(pb: &mut PB, sh: TyId, pi: TyId) -> DefId {
    let t = pb.t;
    let pv = pb.promise(t.unit);
    let mut f = FB::new("worker", pi);
    let id = f.param("id", t.i64, PassMode::Copy);
    let counter = f.param("counter", sh, PassMode::Owned);
    let i = f.local("i", t.i64);
    let add = intr(I::SharedAdd, vec![f.bw(counter), int(1, t.i64)], t.i64);
    let body = vec![
        count_to(&f, i, 1000, t, vec![se(add)]),
        se(await_(intr(I::YieldNow, vec![], pv), t.unit)),
        ret(Some(f.cp(id))),
    ];
    pb.add_fn(f.build_async(body))
}

pub(super) fn tasks() -> Program {
    let mut pb = PB::new();
    let t = pb.t;
    let pre = prelude(&mut pb);
    let mutex = mutex_def(&mut pb);
    let (sh, pi, pv) = {
        let sh = pb.shared(t.i64);
        (sh, pb.promise(t.i64), pb.promise(t.unit))
    };
    let worker = worker(&mut pb, sh, pi);
    let (api, apv, ai, av) = (pb.arr(pi), pb.arr(pv), pb.arr(t.i64), pb.arr(t.unit));
    let (pai, pav) = (pb.promise(ai), pb.promise(av));
    let sa = pb.arr(t.str);
    let mx = pb.adt_ty(mutex, vec![sa]);
    let smx = pb.shared(mx);
    let usize_ = t.usize;
    let push_fn = pb.fn_ty(vec![sa], t.unit);
    let len_fn = pb.fn_ty(vec![sa], usize_);
    let add_fn = pb.fn_ty(vec![t.i64, t.i64], t.i64);
    let push_task = closure_def(
        &mut pb,
        "main::{closure#1}",
        t.unit,
        &[],
        &[("v", sa)],
        |f, _, ps| {
            vec![se(intr(
                I::ArrayPush,
                vec![f.bm(ps[0]), s("task", t)],
                t.unit,
            ))]
        },
    );
    let length = closure_def(
        &mut pb,
        "main::{closure#2}",
        usize_,
        &[],
        &[("v", sa)],
        |f, _, ps| vec![ret(Some(intr(I::ArrayLen, vec![f.bw(ps[0])], usize_)))],
    );
    let sum = closure_def(
        &mut pb,
        "main::{closure#3}",
        t.i64,
        &[],
        &[("a", t.i64), ("b", t.i64)],
        |f, _, ps| vec![ret(Some(bin(B::Add, f.cp(ps[0]), f.cp(ps[1]))))],
    );
    let mut f = FB::new("main", pv);
    let counter = f.local("counter", sh);
    let handles = f.local("handles", api);
    let i = f.local("i", t.i64);
    let ids = f.local("ids", ai);
    let log = f.local("log", smx);
    let tasks = f.local("tasks", apv);
    let j = f.local("j", t.i64);
    let l = f.local("l", smx);
    // `async () => { l.with((v) => { v.push("task"); }); }`
    let task = {
        let mut c = FB::new("main::{closure#0}", pv);
        let lin = c.param("l", smx, PassMode::Owned);
        c.captures.push(Capture {
            outer: l,
            inner: lin,
            mode: PassMode::Owned,
            share: false,
        });
        let with = intr(
            I::MutexWith,
            vec![c.bw(lin), closure(push_task, push_fn)],
            t.unit,
        );
        pb.add_fn(c.build_async(vec![se(with)]))
    };
    let spawn_worker = spawn(
        call(
            worker,
            vec![f.cp(i), intr(I::Clone, vec![f.bw(counter)], sh)],
            pi,
        ),
        pi,
    );
    let reduce = call_g(
        pre.reduce,
        vec![t.i64, t.i64],
        vec![f.bw(ids), closure(sum, add_fn), int(0, t.i64)],
        t.i64,
    );
    let body = vec![
        let_(counter, intr(I::SharedNew, vec![int(0, t.i64)], sh)),
        let_(handles, array(vec![], api)),
        count_to(
            &f,
            i,
            100,
            t,
            vec![se(intr(
                I::ArrayPush,
                vec![f.bm(handles), spawn_worker],
                t.unit,
            ))],
        ),
        let_(ids, await_(promise_all(f.mv(handles), pai), ai)),
        se(print(
            vec![reduce, intr(I::SharedGet, vec![f.bw(counter)], t.i64)],
            t,
        )),
        let_(
            log,
            intr(
                I::SharedNew,
                vec![intr(I::MutexNew, vec![array(vec![], sa)], mx)],
                smx,
            ),
        ),
        let_(tasks, array(vec![], apv)),
        count_to(
            &f,
            j,
            3,
            t,
            vec![
                let_(l, intr(I::Clone, vec![f.bw(log)], smx)),
                se(intr(
                    I::ArrayPush,
                    vec![f.bm(tasks), spawn(closure(task, pb.fn_ty(vec![], pv)), pv)],
                    t.unit,
                )),
            ],
        ),
        se(await_(promise_all(f.mv(tasks), pav), av)),
        se(print(
            vec![intr(
                I::MutexWith,
                vec![f.bw(log), closure(length, len_fn)],
                usize_,
            )],
            t,
        )),
    ];
    pb.add_main(f.build_async(body));
    pb.finish()
}

/// Types shared by the wrappers: `IoError`, `IoStatus` (void results) and `IoResult<T>`.
pub(super) struct Io {
    pub err: TyId,
    err_def: DefId,
    status: TyId,
    result: DefId,
    code_name: DefId,
}

pub(super) fn io_types(pb: &mut PB) -> Io {
    let t = pb.t;
    let err_def = pb.add_def(Def::Adt(adt(
        "IoError",
        AdtKind::Struct,
        vec![("code", t.str, None), ("message", t.str, None)],
    )));
    let err = pb.adt_ty(err_def, vec![]);
    let status_def = pb.add_def(Def::Adt(adt(
        "IoStatus",
        AdtKind::Struct,
        vec![("code", t.i32, None), ("message", t.str, None)],
    )));
    let status = pb.adt_ty(status_def, vec![]);
    let p0 = pb.param(0);
    let mut r = adt(
        "IoResult",
        AdtKind::Struct,
        vec![
            ("code", t.i32, None),
            ("message", t.str, None),
            ("value", p0, None),
        ],
    );
    r.generics = 1;
    let result = pb.add_def(Def::Adt(r));
    let code_name = extern_fn(pb, "velt_rt_err_code_name", vec![t.i32], t.str, false);
    Io {
        err,
        err_def,
        status,
        result,
        code_name,
    }
}

pub(super) fn extern_fn(
    pb: &mut PB,
    sym: &str,
    params: Vec<TyId>,
    ret: TyId,
    is_async: bool,
) -> DefId {
    pb.add_def(Def::ExternFn(ExternFnDef {
        name: sym.into(),
        symbol: sym.into(),
        params,
        ret,
        is_async,
        span: SP,
    }))
}

/// `async function <name>(params) { const r = await <ext>(params); if (r.code != 0) throw
/// IoError { code: codeName(r.code), message: r.message }; return r.value; }` (no `value` for
/// `IoStatus` results).
pub(super) fn wrapper(
    pb: &mut PB,
    io: &Io,
    name: &str,
    sym: &str,
    params: &[(&str, TyId)],
    value: Option<TyId>,
) -> DefId {
    let t = pb.t;
    let res = match value {
        Some(v) => pb.adt_ty(io.result, vec![v]),
        None => io.status,
    };
    let pres = pb.promise(res);
    let ext = extern_fn(pb, sym, params.iter().map(|p| p.1).collect(), pres, true);
    let result_ty = value.unwrap_or(t.unit);
    let pret = pb.promise(result_ty);
    let mut f = FB::new(name, pret);
    f.throws = Some(io.err);
    let ps: Vec<_> = params
        .iter()
        .map(|(n, ty)| {
            let mode = if *ty == t.str {
                PassMode::Owned
            } else {
                PassMode::Copy
            };
            f.param(n, *ty, mode)
        })
        .collect();
    let r = f.local("r", res);
    let args = ps
        .iter()
        .zip(params)
        .map(|(&p, (_, ty))| if *ty == t.str { f.bw(p) } else { f.cp(p) })
        .collect();
    let code = || field(f.bw(r), 0, U::Copy, t.i32);
    let err = adt_lit(
        io.err_def,
        vec![
            call(io.code_name, vec![code()], t.str),
            field(f.bw(r), 1, U::Move, t.str),
        ],
        io.err,
    );
    let mut body = vec![
        let_(r, await_(call(ext, args, pres), res)),
        if_(
            cmp(B::NotEq, code(), int(0, t.i32), t),
            vec![se(throw(err, t.never))],
            None,
        ),
    ];
    if let Some(v) = value {
        body.push(ret(Some(field(f.bw(r), 2, U::Move, v))));
    }
    pb.add_fn(f.build_async(body))
}

pub(super) fn tcp_echo() -> Program {
    let mut pb = PB::new();
    let t = pb.t;
    let io = io_types(&mut pb);
    let u64t = pb.ty(TyKind::Int(IntTy::U64));
    let u32t = pb.ty(TyKind::Int(IntTy::U32));
    let listen = wrapper(
        &mut pb,
        &io,
        "listen",
        "velt_rt_tcp_listen",
        &[("addr", t.str)],
        Some(u64t),
    );
    let connect = wrapper(
        &mut pb,
        &io,
        "connect",
        "velt_rt_tcp_connect",
        &[("addr", t.str)],
        Some(u64t),
    );
    let accept = wrapper(
        &mut pb,
        &io,
        "accept",
        "velt_rt_tcp_accept",
        &[("l", u64t)],
        Some(u64t),
    );
    let read = wrapper(
        &mut pb,
        &io,
        "readString",
        "velt_rt_tcp_read_string",
        &[("s", u64t), ("max", u64t)],
        Some(t.str),
    );
    let write = wrapper(
        &mut pb,
        &io,
        "write",
        "velt_rt_tcp_write",
        &[("s", u64t), ("data", t.str)],
        None,
    );
    let close = extern_fn(&mut pb, "velt_rt_tcp_close", vec![u64t], t.unit, false);
    let port_of = extern_fn(
        &mut pb,
        "velt_rt_tcp_listener_port",
        vec![u64t],
        u32t,
        false,
    );
    let (pu, ps, pv) = (pb.promise(u64t), pb.promise(t.str), pb.promise(t.unit));
    let mut f = FB::new("main", pv);
    f.throws = Some(io.err);
    let server = f.local("server", u64t);
    let port = f.local("port", u32t);
    let task = f.local("serverTask", pv);
    let client = f.local("client", u64t);
    let reply = f.local("reply", t.str);
    let server_task = {
        let mut c = FB::new("main::{closure#0}", pv);
        let srv = c.param("server", u64t, PassMode::Copy);
        c.captures.push(Capture {
            outer: server,
            inner: srv,
            mode: PassMode::Copy,
            share: false,
        });
        let conn = c.local("conn", u64t);
        let data = c.local("data", t.str);
        let e = c.local("e", io.err);
        let body = vec![try_(
            vec![
                let_(conn, await_(call(accept, vec![c.cp(srv)], pu), u64t)),
                let_(
                    data,
                    await_(call(read, vec![c.cp(conn), int(0, u64t)], ps), t.str),
                ),
                se(await_(
                    call(
                        write,
                        vec![c.cp(conn), concat(s("echo: ", t), c.bw(data), t)],
                        pv,
                    ),
                    t.unit,
                )),
                se(call(close, vec![c.cp(conn)], t.unit)),
            ],
            Some((Some(e), vec![se(print(vec![s("server error", t)], t))])),
            None,
        )];
        pb.add_fn(c.build_async(body))
    };
    let body = vec![
        let_(
            server,
            await_(call(listen, vec![s("127.0.0.1:0", t)], pu), u64t),
        ),
        let_(port, call(port_of, vec![f.cp(server)], u32t)),
        let_(task, spawn(closure(server_task, pb.fn_ty(vec![], pv)), pv)),
        let_(
            client,
            await_(
                call(
                    connect,
                    vec![concat(s("127.0.0.1:", t), to_s(f.cp(port), t), t)],
                    pu,
                ),
                u64t,
            ),
        ),
        se(await_(
            call(write, vec![f.cp(client), s("ping", t)], pv),
            t.unit,
        )),
        let_(
            reply,
            await_(call(read, vec![f.cp(client), int(0, u64t)], ps), t.str),
        ),
        se(print(vec![f.bw(reply)], t)),
        se(await_(f.mv(task), t.unit)),
    ];
    pb.add_main(f.build_async(body));
    pb.finish()
}
