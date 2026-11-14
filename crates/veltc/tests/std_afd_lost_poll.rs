//! Windows' reactor (std/lang/hooks/reactor_afd.velt, reactor_afd_poll.velt) keeps one AFD poll
//! request per socket. A request is dropped when a poll or the submission of the next one fails,
//! and nothing woke the socket's waiters again: a listener's `accept` then waited forever for
//! connections that had come (#2905). Now the completion whose resubmission failed wakes the
//! waiters, and the next operation that finds the socket not ready asks for a new request
//! (`rearm`).
//!
//! White-box, in a copy of std: the test makes every poll submitted for the listener's request
//! fail by giving the request a base socket that is no socket. A request made anew finds the real
//! one again. One worker, so the order is the program's: the second client connects once the
//! server waits in its second `accept`. Before the fix both cases waited until the time limit.

mod no_window;
mod runtime_support;
mod test_dir;

use std::path::Path;

fn manifest() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

const SRC: &str = r#"import { describe, expect, it } from "velt/test";
import { connect, TcpStream } from "velt/net";
import * as fs from "velt/net/fd_socket";
import { channel, Channel } from "velt/sync";
import * as io from "./sched_io";
import * as m from "./sched_mem";
import * as p from "./reactor_afd_poll";
import * as tb from "./sched_io_table";

/** reactor_afd_poll.velt's `P_BASE`: the base socket its request's polls name. */
const P_BASE: usize = 64;

/** Every poll submitted from now on for listener `l`'s request fails: its base socket is none. */
function breakPolls(l: u64): bool {
  const s = tb.enter(l);
  const r = m.ld(m.ld(s + io.S_IOB) + io.IO_REACTOR);
  p.lock(r);
  const q = p.find(r, m.ld(s + io.S_FD));
  if (q !== 0) {
    m.st(q + P_BASE, 0x7ffffff0);
  }
  p.unlock(r);
  tb.leave(s);
  return q !== 0;
}

/**
 * Accepts two connections on `l`, its polls broken before the first `accept` (`early`: the poll
 * submitted after the first connection's event fails) or between the two (the poll that `accept`
 * asks for again fails, after the pending one was cancelled). Says when the first is accepted.
 */
async function serve(l: u64, early: bool, first: Channel<i64>): Promise<i64> {
  if (early) {
    expect(breakPolls(l)).toBe(true);
  }
  let n: i64 = 0;
  const a = await fs.accept(l);
  if (a.code === 0) {
    n++;
  }
  if (!early) {
    expect(breakPolls(l)).toBe(true);
  }
  await first.send(n);
  const b = await fs.accept(l);
  if (b.code === 0) {
    n++;
  }
  return n;
}

async function twoClients(early: bool): Promise<i64> {
  const l = (await fs.listenOn("127.0.0.1:0")).value;
  const port = fs.portOf(l);
  const first = channel<i64>();
  const server = spawn(serve(l, early, first));
  const c1: TcpStream = await connect(`127.0.0.1:${port}`);
  expect(await first.receive()).toBe(1);
  // The server waits in its second accept by now (one worker): this connection must wake it.
  const c2: TcpStream = await connect(`127.0.0.1:${port}`);
  const n = await server;
  c1.close();
  c2.close();
  fs.closeListener(l);
  return n;
}

describe("a listener whose poll request was lost", () => {
  it("accepts after the poll submitted after an event failed", async () => {
    expect(await twoClients(true)).toBe(2);
  });
  it("accepts after the poll asked for again failed", async () => {
    expect(await twoClients(false)).toBe(2);
  });
});
"#;

#[test]
fn a_listener_whose_poll_request_was_lost_keeps_accepting() {
    if !cfg!(windows) {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let std_copy = dir.path().join("std");
    copy_tree(&manifest().join("../../std"), &std_copy);
    let test = std_copy.join("lang/hooks/afd_lost_poll.test.velt");
    std::fs::write(&test, SRC).unwrap();
    let o = no_window::command(env!("CARGO_BIN_EXE_velt"))
        .arg("test")
        .arg(&test)
        .current_dir(dir.path())
        .env("VELT_STD", &std_copy)
        .env("VELT_THREADS", "1")
        // A hang (the bug) fails in 30 s instead of the runner's 10 minutes.
        .env("VELT_TEST_TIMEOUT", "30000")
        .env_remove("VELT_SCHEDULER")
        .output()
        .unwrap();
    let out = format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(o.status.success() && out.contains("2 passed"), "{out}");
}
