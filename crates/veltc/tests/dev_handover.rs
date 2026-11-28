//! The dev channel's listener handover, between the supervisor's side (`veltc::dev::handover`)
//! and the program's, which is std's (std/dev/handover_posix.velt, handover_win32.velt): a built
//! program under `VELT_DEV_SOCKET` serves the socket the supervisor bound, a connection made before
//! the program started waits in its backlog and is accepted, an `err` reply fails the `listen`
//! with its message, and a channel nobody serves fails it as "cannot reach the supervisor".

use std::io::Read;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use no_window::command;
use veltc::dev::handover::{self, Request, Server};

mod no_window;
mod runtime_support;
mod test_dir;

const PROGRAM: &str = r#"import { listen } from "velt/net";

async function main() {
  const l = await listen("127.0.0.1:0");
  console.log(`port ${l.port}`);
  const s = await l.accept();
  console.log("accepted");
  s.close();
  l.close();
  try {
    await listen("127.0.0.1:1");
    console.log("listened on the refused address");
  } catch (e) {
    console.log(`refused: ${e.message}`);
  }
}
"#;

/// The address the test's supervisor refuses, and its message.
const REFUSED: &str = "127.0.0.1:1";
const REFUSAL: &str = "this address is not handed over";

fn build(dir: &Path) -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    runtime_support::build_native_runtime(manifest);
    std::fs::write(dir.join("served.velt"), PROGRAM).unwrap();
    let exe = dir.join(format!("served{}", if cfg!(windows) { ".exe" } else { "" }));
    let o = command(env!("CARGO_BIN_EXE_velt"))
        .arg("build")
        .arg(dir.join("served.velt"))
        .arg("-o")
        .arg(&exe)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    exe
}

/// Runs `exe` with `VELT_DEV_SOCKET=socket` and gives its output (stdout, then stderr).
fn run(exe: &Path, socket: &std::ffi::OsStr) -> (String, String) {
    let o = command(exe)
        .env("VELT_DEV_SOCKET", socket)
        .output()
        .unwrap();
    let text = |b: &[u8]| String::from_utf8_lossy(b).replace("\r\n", "\n");
    (text(&o.stdout), text(&o.stderr))
}

#[test]
fn a_program_serves_the_socket_the_supervisor_hands_over() {
    let dir = test_dir::TestDir::new();
    let exe = build(dir.path());
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    // Made before the program runs: it waits in the backlog of the socket handed over.
    let mut early = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let mut server = Server::bind().unwrap();
    let socket = server.name().to_os_string();
    let supervisor = thread::spawn(move || {
        let mut asked = vec![];
        for _ in 0..2 {
            let stream = server.accept().unwrap();
            match handover::read_request(&stream).unwrap() {
                Request::Listen(addr) if addr == REFUSED => {
                    handover::reply_err(&stream, REFUSAL).unwrap();
                    asked.push(addr);
                }
                Request::Listen(addr) => {
                    handover::reply_ok(&stream, &listener).unwrap();
                    asked.push(addr);
                }
                other => panic!("unexpected request {other:?}"),
            }
        }
        asked
    });
    let (out, err) = run(&exe, &socket);
    assert_eq!(
        out,
        format!("port {port}\naccepted\nrefused: {REFUSAL}\n"),
        "{err}"
    );
    assert_eq!(supervisor.join().unwrap(), ["127.0.0.1:0", REFUSED]);
    // The program's end closed the connection it accepted.
    early
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    let mut rest = vec![];
    assert_eq!(early.read_to_end(&mut rest).unwrap(), 0);
}

#[test]
fn a_channel_nobody_serves_fails_the_listen() {
    let dir = test_dir::TestDir::new();
    let exe = build(dir.path());
    let socket = if cfg!(windows) {
        r"\\.\pipe\velt-dev-nobody-serves-this".into()
    } else {
        dir.path().join("nobody.sock").into_os_string()
    };
    let (out, err) = run(&exe, &socket);
    assert_eq!(out, "", "{err}");
    assert!(
        err.contains("velt dev: cannot reach the supervisor: "),
        "{err}"
    );
}
