//! The executor in Velt's workers with sockets (std/lang/hooks/sched.velt, #2649 step 3): a
//! socket served from another worker, a listener's connections spread over the workers, and a
//! connection or server that moves between them. Each program is built with and without
//! `VELT_SCHEDULER=std`, both binaries run with `VELT_THREADS=4` and must print the same.

mod no_window;
mod runtime_support;
mod sched_workers_support;
mod test_dir;

use sched_workers_support::{build_and_run_with, has_threads, manifest, same_on_both};

#[test]
fn a_socket_serves_from_another_worker() {
    if !has_threads() {
        return;
    }
    same_on_both(
        "socket",
        r#"import { listen, connect } from "velt/net";
import { channel, Channel } from "velt/sync";
import { currentWorker } from "velt/task";

async function serve(ports: Channel<i64>): Promise<string> {
  const l = await listen("127.0.0.1:0");
  await ports.send(l.port as i64);
  const s = await l.accept();
  const got = await s.readString();
  await s.write(`echo ${got} from ${currentWorker()}`);
  s.close();
  l.close();
  return got;
}

async function main() {
  const ports = channel<i64>();
  const server = spawn(serve(ports), { worker: 2 });
  const port = (await ports.receive()) ?? 0;
  const c = await connect(`127.0.0.1:${port}`);
  await c.write("hi");
  c.shutdown();
  console.log(await c.readString());
  c.close();
  console.log(`server got ${await server}`);
}
"#,
        "echo hi from 2\nserver got hi\n",
    );
}

/// A listener's connections can be spread over every worker (`TcpListener.workers()`, what
/// http-server's `serve` spreads over): each served on a task pinned to one (it was 1 with the
/// executor in Velt, whose sockets once stayed on worker 0).
#[test]
fn a_listener_spreads_its_connections_over_every_worker() {
    if !has_threads() {
        return;
    }
    same_on_both(
        "spread",
        r#"import { listen, connect, TcpStream } from "velt/net";
import { currentWorker } from "velt/task";

async function answer(s: TcpStream): Promise<void> {
  await s.readString();
  await s.write(`${currentWorker()}`);
  s.close();
}

async function main() {
  const l = await listen("127.0.0.1:0");
  const n = l.workers() as i64;
  let seen = "";
  for (let i: i64 = 0; i < n; i++) {
    const c = await connect(`127.0.0.1:${l.port}`);
    const served = spawn(answer(await l.accept()), { worker: i });
    await c.write("hi");
    seen += `${await c.readString()} `;
    await served;
    c.close();
  }
  console.log(`workers ${n}: ${seen}`);
}
"#,
        "workers 4: 0 1 2 3 \n",
    );
}

/// A program with module state runs its tasks on one worker (one lock, #2748), so its listener
/// says one worker (std/net/tcp_sched.velt `listenerWorkers`), as the default executor's does.
#[test]
fn a_listener_of_a_program_with_module_state_says_one_worker() {
    if !has_threads() {
        return;
    }
    same_on_both(
        "module_state_workers",
        r#"import { listen } from "velt/net";

let counter = 0;

async function main() {
  const l = await listen("127.0.0.1:0");
  counter++;
  console.log(`workers ${l.workers()} ${counter}`);
  l.close();
}
"#,
        "workers 1 1\n",
    );
}

/// A connection served on one worker moves to another mid-stream (its stream handed to a task
/// pinned there): it keeps serving, its registration follows the task to that worker's reactor
/// (#2370), and the client runs on a third worker.
#[test]
fn a_connection_moves_to_another_worker_and_keeps_serving() {
    if !has_threads() {
        return;
    }
    same_on_both(
        "moving",
        r#"import { listen, connect, TcpStream } from "velt/net";
import { channel, Channel } from "velt/sync";
import { currentWorker } from "velt/task";

async function answer(s: TcpStream, from: i64, n: i64): Promise<void> {
  for (let i = from; i < from + n; i++) {
    const got = await s.readString();
    await s.write(`${got} -> pong ${i} on ${currentWorker()}`);
  }
}

async function serveRest(s: TcpStream): Promise<void> {
  await answer(s, 3, 3);
  s.close();
}

async function serve(ports: Channel<i64>): Promise<void> {
  const l = await listen("127.0.0.1:0");
  await ports.send(l.port as i64);
  for (let k = 0; k < 2; k++) {
    const s = await l.accept();
    await answer(s, 0, 3);
    await spawn(serveRest(s), { worker: 2 });
  }
  l.close();
}

async function client(port: i64, k: i64): Promise<string[]> {
  const c = await connect(`127.0.0.1:${port}`);
  const out: string[] = [];
  for (let i = 0; i < 6; i++) {
    await c.write(`ping ${k}.${i}`);
    out.push(await c.readString());
  }
  c.close();
  return out;
}

async function main() {
  const ports = channel<i64>();
  const server = spawn(serve(ports), { worker: 1 });
  const port = (await ports.receive()) ?? 0;
  for (let k = 0; k < 2; k++) {
    for (const line of await spawn(client(port, k), { worker: 3 })) {
      console.log(line);
    }
  }
  await server;
}
"#,
        "ping 0.0 -> pong 0 on 1\nping 0.1 -> pong 1 on 1\nping 0.2 -> pong 2 on 1\n\
         ping 0.3 -> pong 3 on 2\nping 0.4 -> pong 4 on 2\nping 0.5 -> pong 5 on 2\n\
         ping 1.0 -> pong 0 on 1\nping 1.1 -> pong 1 on 1\nping 1.2 -> pong 2 on 1\n\
         ping 1.3 -> pong 3 on 2\nping 1.4 -> pong 4 on 2\nping 1.5 -> pong 5 on 2\n",
    );
}

/// A server task waiting in `accept` on a worker that then runs on without a pause: the stall
/// monitor turns that worker's reactor for it and moves the woken server to another worker,
/// where it accepts, reads and answers (its new connections registered there); with the rescue
/// off (`VELT_RESCUE=0`) the clients would wait for ever.
#[test]
fn a_server_moved_off_a_stalled_worker_keeps_serving() {
    if !has_threads() {
        return;
    }
    same_on_both(
        "rescue",
        r#"import { listen, connect, TcpListener } from "velt/net";

const DONE = new Atomic<i64>(0);

async function serve(l: TcpListener, n: i64): Promise<i64> {
  let served: i64 = 0;
  for (let k = 0; k < n; k++) {
    const s = await l.accept();
    const got = await s.readString();
    await s.write(`echo ${got}`);
    s.close();
    served++;
  }
  l.close();
  return served;
}

async function client(port: i64, k: i64): Promise<string> {
  const c = await connect(`127.0.0.1:${port}`);
  await c.write(`c${k}`);
  c.shutdown();
  const got = await c.readString();
  c.close();
  DONE.add(1);
  return got;
}

async function main() {
  const l = await listen("127.0.0.1:0");
  const port = l.port as i64;
  const server = spawn(serve(l, 3));
  // The server starts and waits in accept, its listener on this worker's reactor.
  await yieldNow();
  const clients: Promise<string>[] = [];
  for (let k = 0; k < 3; k++) {
    clients.push(spawn(client(port, k), { worker: 1 }));
  }
  // This worker runs on without a pause until every client has its answer: only a server moved
  // to another worker can give it.
  while (DONE.load() < 3) {}
  for (const line of await Promise.all(clients)) {
    console.log(line);
  }
  console.log(`served ${await server}`);
}
"#,
        "echo c0\necho c1\necho c2\nserved 3\n",
    );
}

/// A listener opened by `main` accepts and reads in a task pinned to another worker: the handle
/// resolves in the process's table, and the connection's registration follows the task.
#[test]
fn a_listener_opened_on_worker_0_accepts_on_another() {
    if !has_threads() {
        return;
    }
    same_on_both(
        "listener",
        r#"import { listen, connect, TcpListener } from "velt/net";

async function serve(l: TcpListener): Promise<string> {
  const s = await l.accept();
  const got = await s.readString();
  await s.write(`echo ${got}`);
  s.close();
  return got;
}

async function main() {
  const l = await listen("127.0.0.1:0");
  const port = l.port;
  const server = spawn(serve(l), { worker: 1 });
  const c = await connect(`127.0.0.1:${port}`);
  await c.write("hi");
  c.shutdown();
  console.log(await c.readString());
  c.close();
  console.log(`server ${await server}`);
  l.close();
}
"#,
        "echo hi\nserver hi\n",
    );
}

/// A socket that a second task uses (a pool's connection, which tasks take turns with) stays on
/// its reactor, and the task follows it: here the task, spawned from worker 2, uses the
/// connection `main` used last, and its read's reply wakes it on the worker whose reactor has the
/// connection, not across threads. That worker is `main`'s at its last read (the read moves the
/// connection to `main`'s home, whose reactor wakes it), which the spread may have made another
/// than worker 0 on a loaded machine, so the program prints whether the task ended up there, not
/// a worker's index. The stall monitor is off (`VELT_RESCUE=0`): the rule is asserted only so, since
/// with the rescue on (the default) the monitor may move the task (1 of 60 `false` on contended cores). With `join` removed (the task
/// stays on worker 2) it prints `false` (60 of 60 runs on two contended cores, Linux arm64 VM; 60 of 60 `true` with it).
/// The server answers the task 20 ms after its request, so the answer wakes the task: one there
/// already when the task reads would leave it where it is.
#[test]
fn a_task_follows_a_socket_tasks_take_turns_with() {
    if !has_threads() {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let (code, out) = build_and_run_with(
        dir.path(),
        "follows",
        r#"import { listen, connect, TcpStream } from "velt/net";
import { channel, Channel } from "velt/sync";
import { currentWorker } from "velt/task";

async function echo(ports: Channel<i64>): Promise<void> {
  const l = await listen("127.0.0.1:0");
  await ports.send(l.port as i64);
  const s = await l.accept();
  for (let i = 0; i < 2; i++) {
    const got = await s.readString();
    if (i == 1) {
      // The reply wakes `user` (it waits for it by then), rather than being there already.
      await sleep(20);
    }
    await s.write(`${got}!`);
  }
  s.close();
  l.close();
}

async function user(c: TcpStream, socketWorker: i64): Promise<string> {
  await c.write("b");
  const got = await c.readString();
  const here = currentWorker();
  c.close();
  return `${got} on the connection's worker: ${here == socketWorker}`;
}

async function starter(c: TcpStream, socketWorker: i64): Promise<string> {
  return await spawn(user(c, socketWorker));
}

async function main() {
  const ports = channel<i64>();
  const server = spawn(echo(ports), { worker: 1 });
  const port = (await ports.receive()) ?? 0;
  // A sleep anchors `main` to the worker it runs on (it may run on worker 1, woken there by the
  // send): its connection then lives on that worker's reactor, the one `currentWorker()` names.
  await sleep(1);
  const c = await connect(`127.0.0.1:${port}`);
  await c.write("a");
  console.log(await c.readString());
  const socketWorker = currentWorker();
  console.log(await spawn(starter(c, socketWorker), { worker: 2 }));
  await server;
}
"#,
        true,
        "4",
        &[("VELT_RESCUE", "0")],
    );
    assert_eq!(
        (code, out.as_str()),
        (0, "a!\nb! on the connection's worker: true\n")
    );
}
