//! The shape of the M4 golden `http_server.vlt` as HIR (shared by the interpreter tests and the
//! native tests): std/http's `serve` adapter — an async closure `(raw: u64) => Promise<u64>`
//! given to `__intrinsic_http_handler`, capturing the user's handler — and a user handler
//! capturing a `shared` hit counter, called once per request. `main` serves on port 0, prints
//! the port, waits until three requests were handled, prints the count and closes the server.
//!
//! ```text
//! struct Req { method: string; path: string; body: string }
//! async function main() {
//!   const hits = shared(0);
//!   const counter = hits.clone();
//!   const handler = async (req: Req): Promise<u64> => {
//!     const n = counter.add(1);
//!     let text = `${req.method} ${req.path} #${n} ${req.body}`;
//!     const r = velt_rt_http_resp_new(req.path == "/missing" ? 404 : 200);
//!     velt_rt_http_resp_body_text(r, text);
//!     return r;
//!   };
//!   const h = __intrinsic_http_handler(async (raw: u64): Promise<u64> => {
//!     const req = Req { method: req_method(raw), path: req_path(raw), body: req_body(raw) };
//!     velt_rt_http_req_drop(raw);
//!     const resp = await handler(req);
//!     return resp;
//!   });
//!   const res = await velt_rt_http_serve("127.0.0.1:0", h);
//!   const srv = res.value;
//!   console.log("listening", velt_rt_http_server_port(srv));
//!   while (hits.get() < 3) await sleep(5);
//!   console.log("hits", hits.get());
//!   velt_rt_http_server_close(srv);
//! }
//! ```

use velt_sema::hir::{
    AdtKind, BinOp as B, Capture, Def, DefId, IntTy, Intrinsic as I, PassMode, Program, TyId,
    TyKind, UseMode as U,
};

use super::builder::*;
use super::builder_m2::*;
use super::builder_m3::*;
use super::programs_m3::extern_fn;

struct Types {
    u64t: TyId,
    u32t: TyId,
    sh: TyId,
    req: TyId,
    req_def: DefId,
    pu: TyId,
}

/// `(method, path, body) -> string` request accessors, then drop, resp_new, resp_body_text.
struct Rt {
    method: DefId,
    path: DefId,
    body: DefId,
    req_drop: DefId,
    resp_new: DefId,
    resp_text: DefId,
}

fn rt_fns(pb: &mut PB, ty: &Types) -> Rt {
    let t = pb.t;
    let str_of = |pb: &mut PB, sym: &str| extern_fn(pb, sym, vec![ty.u64t], t.str, false);
    Rt {
        method: str_of(pb, "velt_rt_http_req_method"),
        path: str_of(pb, "velt_rt_http_req_path"),
        body: str_of(pb, "velt_rt_http_req_body"),
        req_drop: extern_fn(pb, "velt_rt_http_req_drop", vec![ty.u64t], t.unit, false),
        resp_new: extern_fn(pb, "velt_rt_http_resp_new", vec![ty.u32t], ty.u64t, false),
        resp_text: extern_fn(
            pb,
            "velt_rt_http_resp_body_text",
            vec![ty.u64t, t.str],
            t.unit,
            false,
        ),
    }
}

/// The user handler: `async (req: Req): Promise<u64>` capturing `counter` (moved in).
fn user_handler(pb: &mut PB, ty: &Types, rt: &Rt, counter: velt_sema::hir::LocalId) -> DefId {
    let t = pb.t;
    let mut c = FB::new("main::{closure#0}", ty.pu);
    let cnt = c.param("counter", ty.sh, PassMode::Owned);
    c.captures.push(Capture {
        outer: counter,
        inner: cnt,
        mode: PassMode::Owned,
        clone: false,
    });
    let req = c.param("req", ty.req, PassMode::Owned);
    let n = c.local("n", t.i64);
    let text = c.local("text", t.str);
    let r = c.local("r", ty.u64t);
    let fld = |i| field(c.bw(req), i, U::Borrow, t.str);
    let line = concat(
        concat(concat(fld(0), s(" ", t), t), fld(1), t),
        concat(
            concat(s(" #", t), to_s(c.cp(n), t), t),
            concat(s(" ", t), fld(2), t),
            t,
        ),
        t,
    );
    let status = ifx(
        cmp(B::Eq, fld(1), s("/missing", t), t),
        int(404, ty.u32t),
        int(200, ty.u32t),
    );
    let body = vec![
        let_(n, intr(I::SharedAdd, vec![c.bw(cnt), int(1, t.i64)], t.i64)),
        let_(text, line),
        let_(r, call(rt.resp_new, vec![status], ty.u64t)),
        se(call(rt.resp_text, vec![c.cp(r), c.bm(text)], t.unit)),
        ret(Some(c.cp(r))),
    ];
    pb.add_fn(c.build_async(body))
}

/// std/http's adapter: `async (raw: u64): Promise<u64>` capturing `handler` (moved in).
fn adapter(pb: &mut PB, ty: &Types, rt: &Rt, handler: velt_sema::hir::LocalId) -> DefId {
    let t = pb.t;
    let hty = pb.fn_ty(vec![ty.req], ty.pu);
    let mut c = FB::new("serve::{closure#0}", ty.pu);
    let h = c.param("handler", hty, PassMode::Owned);
    c.captures.push(Capture {
        outer: handler,
        inner: h,
        mode: PassMode::Owned,
        clone: false,
    });
    let raw = c.param("raw", ty.u64t, PassMode::Copy);
    let req = c.local("req", ty.req);
    let resp = c.local("resp", ty.u64t);
    let get = |f: DefId| call(f, vec![c.cp(raw)], t.str);
    let body = vec![
        let_(
            req,
            adt_lit(
                ty.req_def,
                vec![get(rt.method), get(rt.path), get(rt.body)],
                ty.req,
            ),
        ),
        se(call(rt.req_drop, vec![c.cp(raw)], t.unit)),
        let_(
            resp,
            await_(call_ptr(c.bw(h), vec![c.bw(req)], ty.pu), ty.u64t),
        ),
        ret(Some(c.cp(resp))),
    ];
    pb.add_fn(c.build_async(body))
}

pub(super) fn http_server() -> Program {
    let mut pb = PB::new();
    let t = pb.t;
    let u64t = pb.ty(TyKind::Int(IntTy::U64));
    let req_def = pb.add_def(Def::Adt(adt(
        "Req",
        AdtKind::Struct,
        vec![
            ("method", t.str, None),
            ("path", t.str, None),
            ("body", t.str, None),
        ],
    )));
    let ty = Types {
        u64t,
        u32t: pb.ty(TyKind::Int(IntTy::U32)),
        sh: pb.shared(t.i64),
        req: pb.adt_ty(req_def, vec![]),
        req_def,
        pu: pb.promise(u64t),
    };
    let rt = rt_fns(&mut pb, &ty);
    let tup = pb.ty(TyKind::Tuple(vec![u64t; 6]));
    let p0 = pb.param(0);
    let mut io = adt(
        "IoResult",
        AdtKind::Struct,
        vec![
            ("code", t.i32, None),
            ("message", t.str, None),
            ("value", p0, None),
        ],
    );
    io.generics = 1;
    let io_def = pb.add_def(Def::Adt(io));
    let io_u64 = pb.adt_ty(io_def, vec![u64t]);
    let p_io = pb.promise(io_u64);
    let serve = extern_fn(&mut pb, "velt_rt_http_serve", vec![t.str, tup], p_io, true);
    let port_of = extern_fn(
        &mut pb,
        "velt_rt_http_server_port",
        vec![u64t],
        ty.u32t,
        false,
    );
    let close = extern_fn(
        &mut pb,
        "velt_rt_http_server_close",
        vec![u64t],
        t.unit,
        false,
    );
    let pv = pb.promise(t.unit);
    let hty = pb.fn_ty(vec![ty.req], ty.pu);
    let aty = pb.fn_ty(vec![u64t], ty.pu);

    let mut f = FB::new("main", pv);
    let hits = f.local("hits", ty.sh);
    let counter = f.local("counter", ty.sh);
    let handler = f.local("handler", hty);
    let h = f.local("h", tup);
    let res = f.local("res", io_u64);
    let srv = f.local("srv", u64t);
    let user = user_handler(&mut pb, &ty, &rt, counter);
    let adapt = adapter(&mut pb, &ty, &rt, handler);
    let hits_now = || intr(I::SharedGet, vec![f.bw(hits)], t.i64);
    let body = vec![
        let_(hits, intr(I::SharedNew, vec![int(0, t.i64)], ty.sh)),
        let_(counter, intr(I::Clone, vec![f.bw(hits)], ty.sh)),
        let_(handler, closure(user, hty)),
        let_(h, intr(I::HttpHandler, vec![closure(adapt, aty)], tup)),
        let_(
            res,
            await_(
                call(serve, vec![s("127.0.0.1:0", t), f.bw(h)], p_io),
                io_u64,
            ),
        ),
        let_(srv, field(f.bw(res), 2, U::Copy, u64t)),
        se(print(
            vec![s("listening", t), call(port_of, vec![f.cp(srv)], ty.u32t)],
            t,
        )),
        while_(
            None,
            cmp(B::Lt, hits_now(), int(3, t.i64), t),
            vec![se(await_(sleep(int(5, t.i64), pv), t.unit))],
            None,
        ),
        se(print(vec![s("hits", t), hits_now()], t)),
        se(call(close, vec![f.cp(srv)], t.unit)),
    ];
    pb.add_main(f.build_async(body));
    pb.finish()
}
