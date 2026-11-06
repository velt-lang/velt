//! HIR programs of the interpreter tests: the M3 golden `tasks` and std-like wrappers over
//! `declare async function` rt externs. (The native tests, `tests/native.rs`, link the real
//! runtime, which prints no integer: they load the golden sources with the real std, std_src.rs,
//! #2197.)

use velt_sema::hir::{
    AdtKind, BinOp as B, Capture, Def, DefId, ExternFnDef, Intrinsic as I, PassMode, Program, TyId,
    TyKind, UseMode as U,
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
    let code_name = extern_fn(pb, "test_err_code_name", vec![t.i32], t.str, false);
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
        system: None,
        host: None,
        c: None,
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
