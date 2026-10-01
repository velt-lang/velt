//! Emulated rt I/O (rt_abi_async.md §3, §5, §6) for the interpreter: an in-memory file map and
//! in-process TCP (listeners, connected stream pairs with byte inboxes). Operations that must
//! wait (accept, read) are `Fut::Io` futures that become ready when another task acts; every
//! state change sets `woke` so the executor polls again before advancing its clock.
//! Handles are fake pointers (never dereferenced). `IoResult<T>` = `{ code: i32, message:
//! string @8, value: T @32 }`.

use std::collections::{HashMap, VecDeque};

use super::async_rt::Fut;
use super::Interp;

const HANDLE_BASE: u64 = 0x7000_0000;

/// A pending I/O future.
pub(super) enum IoOp {
    Accept { listener: u64 },
    ReadString { stream: u64 },
}

struct Stream {
    peer: u64,
    inbox: Vec<u8>,
    peer_closed: bool,
}

#[derive(Default)]
pub(super) struct Net {
    next: u64,
    /// Listener handle → (port, accepted-but-unclaimed server-side streams).
    listeners: HashMap<u64, (u32, VecDeque<u64>)>,
    streams: HashMap<u64, Stream>,
    files: HashMap<String, Vec<u8>>,
}

impl Net {
    fn handle(&mut self) -> u64 {
        self.next += 1;
        HANDLE_BASE + self.next * 16
    }
}

impl Interp<'_> {
    /// Emulated fs/net functions; `None` if `sym` is not one of them.
    pub(super) fn rt_net(&mut self, sym: &str, a: &[u64]) -> Option<Result<u64, i32>> {
        Some(Ok(match sym {
            "velt_rt_tcp_listen" => {
                let port = 4000 + self.exec.net.listeners.len() as u32;
                let l = self.exec.net.handle();
                self.exec.net.listeners.insert(l, (port, VecDeque::new()));
                self.ready_io(40, None, l)
            }
            "velt_rt_tcp_listener_port" => self.exec.net.listeners[&a[0]].0 as u64,
            "velt_rt_tcp_listener_close" => {
                self.exec.net.listeners.remove(&a[0]);
                0
            }
            "velt_rt_tcp_accept" => self.new_fut(40, Fut::Io(IoOp::Accept { listener: a[0] })),
            "velt_rt_tcp_connect" => self.tcp_connect(a[0]),
            "velt_rt_tcp_read_string" => {
                self.new_fut(56, Fut::Io(IoOp::ReadString { stream: a[0] }))
            }
            "velt_rt_tcp_write" | "velt_rt_tcp_write_bytes" => {
                let data = self.str_bytes(a[1]);
                let peer = self.exec.net.streams[&a[0]].peer;
                if let Some(p) = self.exec.net.streams.get_mut(&peer) {
                    p.inbox.extend(data);
                }
                self.exec.woke = true;
                self.ready_io(32, None, 0)
            }
            "velt_rt_tcp_close" => {
                if let Some(s) = self.exec.net.streams.remove(&a[0]) {
                    if let Some(p) = self.exec.net.streams.get_mut(&s.peer) {
                        p.peer_closed = true;
                    }
                }
                self.exec.woke = true;
                0
            }
            _ => return self.rt_fs(sym, a),
        }))
    }

    fn rt_fs(&mut self, sym: &str, a: &[u64]) -> Option<Result<u64, i32>> {
        Some(Ok(match sym {
            "velt_rt_fs_write_file" => {
                let path = self.str_text(a[0]);
                let data = self.str_bytes(a[1]);
                self.exec.net.files.insert(path, data);
                self.ready_io(32, None, 0)
            }
            "velt_rt_fs_read_file" => {
                let path = self.str_text(a[0]);
                let f = self.new_fut(56, Fut::Ready);
                self.read_file_into(&path, f + 16);
                f
            }
            "velt_rt_fs_read_file_sync" => {
                let path = self.str_text(a[0]);
                self.read_file_into(&path, a[1]) as u64
            }
            "velt_rt_err_code_name" => {
                let name = match a[0] as u32 as i32 {
                    1 => "ENOENT",
                    7 => "ECONNREFUSED",
                    _ => "UNKNOWN",
                };
                self.static_str(a[1], name);
                0
            }
            _ => return None,
        }))
    }

    /// A string whose bytes are never freed (`cap == 0`).
    fn static_str(&mut self, out: u64, s: &str) {
        let p = self.raw_alloc(s.len().max(1) as u64);
        self.write_bytes(p, s.as_bytes());
        self.write_bytes(out, &p.to_le_bytes());
        self.write_bytes(out + 8, &(s.len() as u64).to_le_bytes());
        self.write_bytes(out + 16, &0u64.to_le_bytes());
    }

    /// Write an `IoResult<string>` for reading `path` at `at`; returns the code.
    fn read_file_into(&mut self, path: &str, at: u64) -> i32 {
        self.write_bytes(at, &[0; 56]);
        match self.exec.net.files.get(path).cloned() {
            Some(b) => {
                self.new_str(at + 32, &b);
                0
            }
            None => {
                self.write_bytes(at, &1i32.to_le_bytes());
                self.new_str(at + 8, format!("no such file: {path}").as_bytes());
                1
            }
        }
    }

    /// A completed future holding an `IoResult` of `size` bytes: an error `(code, message)` or
    /// success with a scalar `value` (written when `size > 32`).
    pub(super) fn ready_io(&mut self, size: u64, err: Option<(i32, &str)>, value: u64) -> u64 {
        let f = self.new_fut(size, Fut::Ready);
        self.write_bytes(f + 16, &vec![0; size as usize]);
        match err {
            Some((code, msg)) => {
                self.write_bytes(f + 16, &code.to_le_bytes());
                self.new_str(f + 24, msg.as_bytes());
            }
            None if size > 32 => self.write_bytes(f + 48, &value.to_le_bytes()),
            None => {}
        }
        f
    }

    fn tcp_connect(&mut self, addr: u64) -> u64 {
        let text = self.str_text(addr);
        let port: u32 = text
            .rsplit(':')
            .next()
            .and_then(|p| p.parse().ok())
            .unwrap_or(0);
        let Some(l) = self
            .exec
            .net
            .listeners
            .iter()
            .find(|(_, v)| v.0 == port)
            .map(|(k, _)| *k)
        else {
            return self.ready_io(40, Some((7, "connection refused")), 0);
        };
        let (c, s) = (self.exec.net.handle(), self.exec.net.handle());
        for (me, peer) in [(c, s), (s, c)] {
            let stream = Stream {
                peer,
                inbox: vec![],
                peer_closed: false,
            };
            self.exec.net.streams.insert(me, stream);
        }
        if let Some(v) = self.exec.net.listeners.get_mut(&l) {
            v.1.push_back(s);
        }
        self.exec.woke = true;
        self.ready_io(40, None, c)
    }

    /// Poll a pending I/O future; writes its `IoResult` when ready.
    pub(super) fn poll_io(&mut self, f: u64) -> bool {
        let Some(Fut::Io(op)) = self.exec.futs.get(&f) else {
            unreachable!()
        };
        match *op {
            IoOp::Accept { listener } => {
                let next = self
                    .exec
                    .net
                    .listeners
                    .get_mut(&listener)
                    .and_then(|v| v.1.pop_front());
                match next {
                    Some(s) => {
                        self.write_bytes(f + 16, &[0; 40]);
                        self.write_bytes(f + 48, &s.to_le_bytes());
                        true
                    }
                    None => false,
                }
            }
            IoOp::ReadString { stream } => {
                let s = self.exec.net.streams.get_mut(&stream);
                let Some(s) = s.filter(|s| !s.inbox.is_empty() || s.peer_closed) else {
                    return false;
                };
                let data = std::mem::take(&mut s.inbox);
                self.write_bytes(f + 16, &[0; 56]);
                self.new_str(f + 48, &data);
                true
            }
        }
    }
}
