//! TCP echo: a spawned compiled server task and a compiled client, in-process.

use super::fake::{
    arg, block_on_fut, block_on_state, fut_result, join, ok, spawn_state, take_string,
};
use crate::bytes::{velt_rt_bytes_drop, VeltBytes};
use crate::net::tcp::*;
use crate::result::{code_name, IoResult, VeltErr};
use crate::str::VeltStr;
use crate::task::{drop_fut, poll_fut, VeltFut, PENDING, READY};
use std::ffi::c_void;
use std::ptr::null_mut;

/// Await `st.fut`; on READY move its result out and drop it. `None` ⇒ return PENDING.
unsafe fn await_fut<R>(fut: &mut *mut VeltFut, cx: *mut c_void) -> Option<R> {
    if poll_fut(*fut, cx) == PENDING {
        return None;
    }
    let r = fut_result::<R>(*fut);
    drop_fut(*fut);
    *fut = null_mut();
    Some(r)
}

// async function echo(l): i64 {
//   const s = await accept(l); let total = 0;
//   for (;;) { const b = await s.read(4096); if (b.len == 0) break; total += b.len; await s.write(b); }
//   s.close(); return total;
// }
#[repr(C)]
struct Echo {
    result: i64,
    tag: u32,
    listener: ListenerHandle,
    stream: StreamHandle,
    fut: *mut VeltFut,
}

unsafe extern "C-unwind" fn echo_poll(s: *mut u8, cx: *mut c_void) -> u32 {
    let st = &mut *(s as *mut Echo);
    loop {
        match st.tag {
            0 => (st.fut, st.tag) = (velt_rt_tcp_accept(st.listener), 1),
            1 => {
                let Some(r) = await_fut::<IoResult<StreamHandle>>(&mut st.fut, cx) else {
                    return PENDING;
                };
                (st.stream, st.tag) = (ok(r), 2);
            }
            2 => (st.fut, st.tag) = (velt_rt_tcp_read(st.stream, 4096), 3),
            3 => {
                let Some(r) = await_fut::<IoResult<VeltBytes>>(&mut st.fut, cx) else {
                    return PENDING;
                };
                let mut b = ok(r);
                if b.len == 0 {
                    velt_rt_tcp_close(st.stream);
                    return READY;
                }
                st.result += b.len as i64;
                st.fut = velt_rt_tcp_write_bytes(st.stream, &b);
                velt_rt_bytes_drop(&mut b);
                st.tag = 4;
            }
            4 => {
                let Some(r) = await_fut::<IoResult<()>>(&mut st.fut, cx) else {
                    return PENDING;
                };
                ok(r);
                st.tag = 2;
            }
            _ => unreachable!(),
        }
    }
}

// async function client(port): string {
//   const s = await connect(`127.0.0.1:${port}`); await s.write(MSG); s.shutdown();
//   let acc = ""; for (;;) { const t = await s.readString(3); if (t == "") break; acc += t; }
//   s.close(); return acc;
// }
// Reading 3 bytes at a time splits the 3-byte '→' across reads: readString must reassemble it.
#[repr(C)]
struct Client {
    result: String,
    tag: u32,
    port: u32,
    stream: StreamHandle,
    fut: *mut VeltFut,
}

const MSG: &str = "ping → pong";

unsafe extern "C-unwind" fn client_poll(s: *mut u8, cx: *mut c_void) -> u32 {
    let st = &mut *(s as *mut Client);
    loop {
        match st.tag {
            0 => {
                let addr = format!("127.0.0.1:{}", st.port);
                (st.fut, st.tag) = (velt_rt_tcp_connect(&arg(&addr)), 1);
            }
            1 => {
                let Some(r) = await_fut::<IoResult<StreamHandle>>(&mut st.fut, cx) else {
                    return PENDING;
                };
                st.stream = ok(r);
                let data = arg(MSG);
                st.fut = velt_rt_tcp_write(st.stream, &data);
                st.tag = 2;
            }
            2 => {
                let Some(r) = await_fut::<IoResult<()>>(&mut st.fut, cx) else {
                    return PENDING;
                };
                ok(r);
                let mut e = std::mem::MaybeUninit::<VeltErr>::uninit();
                velt_rt_tcp_shutdown(st.stream, e.as_mut_ptr());
                assert_eq!(e.assume_init().code, 0);
                st.tag = 3;
            }
            3 => (st.fut, st.tag) = (velt_rt_tcp_read_string(st.stream, 3), 4),
            4 => {
                let Some(r) = await_fut::<IoResult<VeltStr>>(&mut st.fut, cx) else {
                    return PENDING;
                };
                let text = take_string(ok(r));
                if text.is_empty() {
                    velt_rt_tcp_close(st.stream);
                    return READY;
                }
                st.result.push_str(&text);
                st.tag = 3;
            }
            _ => unreachable!(),
        }
    }
}

#[test]
fn tcp_echo_in_process() {
    let listener = ok(block_on_fut::<IoResult<ListenerHandle>>(unsafe {
        velt_rt_tcp_listen(&arg("127.0.0.1:0"))
    }));
    let port = unsafe { velt_rt_tcp_listener_port(listener) };
    assert!(port > 0);
    let mut echo = Box::new(Echo {
        result: 0,
        tag: 0,
        listener,
        stream: StreamHandle::NULL,
        fut: null_mut(),
    });
    // SAFETY: `echo` outlives the task, which is joined below; it runs to completion.
    let server = unsafe { spawn_state(echo_poll, &mut *echo as *mut Echo as *mut u8) };
    let mut client = Client {
        result: String::new(),
        tag: 0,
        port,
        stream: StreamHandle::NULL,
        fut: null_mut(),
    };
    unsafe { block_on_state(client_poll, &mut client as *mut Client as *mut u8) };
    assert_eq!(client.result, MSG);
    join(server);
    assert_eq!(echo.result, MSG.len() as i64);
    unsafe { velt_rt_tcp_listener_close(listener) };

    let addr = format!("127.0.0.1:{port}");
    let refused =
        block_on_fut::<IoResult<StreamHandle>>(unsafe { velt_rt_tcp_connect(&arg(&addr)) });
    assert_eq!(code_name(refused.err.code), "ECONNREFUSED");
    take_string(refused.err.message);
}
