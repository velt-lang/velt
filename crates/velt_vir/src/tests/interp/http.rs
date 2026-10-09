//! Emulated std/http server side (rt_abi_async.md §7) for the interpreter. `velt_rt_http_serve`
//! reads the `VeltHandler` and starts one task per scripted fake request (`Http::requests`,
//! set before the run): a heap future whose state `init(env, req, state)` writes, polled with
//! the handler's `poll`. Requests and responses are fake handles; a response records its
//! status and body. `velt_rt_http_server_close` collects the responses of the finished
//! requests (in request order) and releases the handler environment through its drop function
//! — the real runtime never frees it, but doing so here keeps the leak check exact.

use std::collections::HashMap;

use super::async_rt::Fut;
use super::Interp;

const REQ_BASE: u64 = 0x5100_0000;
const RESP_BASE: u64 = 0x5200_0000;
const SERVER: u64 = 0x5300_0000;

#[derive(Default)]
pub(in crate::tests) struct Http {
    /// `(method, path, body)` of the requests to serve.
    pub requests: Vec<(String, String, String)>,
    /// `(status, body)` per served request, filled by `velt_rt_http_server_close`.
    pub responses: Vec<(u32, String)>,
    live_reqs: HashMap<u64, (String, String, String)>,
    resps: HashMap<u64, (u32, String)>,
    tasks: Vec<usize>,
    env: u64,
    next: u64,
}

impl Interp<'_> {
    /// Emulated http server functions; `None` if `sym` is not one of them.
    pub(super) fn rt_http(&mut self, sym: &str, a: &[u64]) -> Option<Result<u64, i32>> {
        Some(Ok(match sym {
            "velt_rt_http_serve" => return Some(self.http_serve(a[1])),
            "velt_rt_http_server_port" => 8080,
            "velt_rt_http_server_close" => return Some(self.http_close().map(|_| 0)),
            "velt_rt_http_req_method" | "velt_rt_http_req_url" => {
                let (m, p, _) = self.exec.http.live_reqs[&a[0]].clone();
                let text = match sym {
                    "velt_rt_http_req_method" => m,
                    // The scripted requests name `Host: localhost`.
                    _ => format!("http://localhost{p}"),
                };
                self.new_str(a[1], text.as_bytes());
                0
            }
            "velt_rt_http_req_drop" => {
                let r = self.exec.http.live_reqs.remove(&a[0]);
                assert!(r.is_some(), "interp: request dropped twice");
                0
            }
            // `(status, reason, headers, kind, text, bytes, implied)`: a text body.
            "velt_rt_http_resp_build" => {
                let h = self.http_handle(RESP_BASE);
                let body = String::from_utf8_lossy(&self.str_bytes(a[4])).into_owned();
                self.exec.http.resps.insert(h, (a[0] as u32, body));
                // The runtime takes the body: the caller's string is left empty.
                if let Err(e) = self.rt("velt_rt_str_drop", &[a[4]]) {
                    return Some(Err(e));
                }
                h
            }
            _ => return None,
        }))
    }

    fn http_handle(&mut self, base: u64) -> u64 {
        self.exec.http.next += 1;
        base + self.exec.http.next * 16
    }

    /// Start a task per scripted request; the result is a ready `IoResult<VeltServer*>`.
    fn http_serve(&mut self, h: u64) -> Result<u64, i32> {
        let d: Vec<u64> = (0..6).map(|i| self.read_u64(h + 8 * i)).collect();
        let (init, poll, drop, size, align, env) = (d[0], d[1], d[2], d[3], d[4], d[5]);
        assert!(
            align <= 16 && size >= 8,
            "interp: handler state {size}/{align}"
        );
        self.exec.http.env = env;
        for r in self.exec.http.requests.clone() {
            let req = self.http_handle(REQ_BASE);
            self.exec.http.live_reqs.insert(req, r);
            let kind = Fut::Boxed {
                poll: self.func_id(poll),
                drop: self.func_id(drop),
                done: false,
            };
            let fut = self.new_fut(size, kind);
            self.call_addr(init, vec![env, req, fut + 16])?;
            let t = self.start_task(fut, 8);
            self.exec.http.tasks.push(t);
        }
        Ok(self.ready_io(40, None, SERVER))
    }

    fn http_close(&mut self) -> Result<(), i32> {
        for t in self.exec.http.tasks.clone() {
            let bytes = self
                .task_result(t)
                .expect("interp: request still running at close");
            let resp = u64::from_le_bytes(bytes[..8].try_into().expect("8 bytes"));
            let r = self
                .exec
                .http
                .resps
                .remove(&resp)
                .unwrap_or((500, String::new()));
            self.exec.http.responses.push(r);
        }
        assert!(
            self.exec.http.live_reqs.is_empty(),
            "interp: handler leaked a request"
        );
        let env = self.exec.http.env;
        if env != 0 {
            let drop = self.read_u64(env);
            if drop != 0 {
                self.call_addr(drop, vec![env])?;
            }
        }
        Ok(())
    }
}
