//! A connect to a loopback port nobody listens on fails at once, as in Node (#3964): Windows sends
//! the SYN again twice after a refusal (about 2 s) unless the socket asks it not to
//! (std/net/sys_win32.velt `beforeConnect`), and `localhost` resolves to `::1` first, so a
//! `fetch` of `http://localhost:<port>/` from a server on 127.0.0.1 only waited 2 s for `::1`
//! before it connected. Linux and macOS refuse at once anyway.
//!
//! The program fetches a closed port five times each on 127.0.0.1, `::1` and `localhost`
//! (`localhost` tries both): fifteen refusals, each about 2 s before the fix, so it ran well over
//! 30 s; with it, the whole program takes about as long as starting it.

use std::path::Path;
use std::time::{Duration, Instant};

use no_window::command;

mod no_window;
mod runtime_support;
mod test_dir;

const PROGRAM: &str = r#"import { IoError } from "velt/io";

const port = process.argv[2];
let refused = 0;
for (const host of ["127.0.0.1", "[::1]", "localhost"]) {
  for (let i = 0; i < 5; i++) {
    try {
      await fetch(`http://${host}:${port}/`);
      console.log(`${host}: connected`);
    } catch (e) {
      // A machine without IPv6 fails `::1` otherwise, as fast.
      if (e instanceof IoError && (e.code == "ECONNREFUSED" || host == "[::1]")) {
        refused++;
      } else {
        console.log(`${host}: ${e}`);
      }
    }
  }
}
console.log(`refused ${refused}`);
"#;

// timing: how long the refusals take is what this test checks (each took about 2 s on Windows).
const LIMIT: Duration = Duration::from_secs(5);

#[test]
fn a_connect_to_a_closed_loopback_port_fails_at_once() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    runtime_support::build_native_runtime(manifest);
    let dir = test_dir::TestDir::new();
    std::fs::write(dir.path().join("refused.velt"), PROGRAM).unwrap();
    let exe = dir.path().join(format!(
        "refused{}",
        if cfg!(windows) { ".exe" } else { "" }
    ));
    let o = command(env!("CARGO_BIN_EXE_velt"))
        .arg("build")
        .arg(dir.path().join("refused.velt"))
        .arg("-o")
        .arg(&exe)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    // A port free a moment ago: nothing listens on it, on 127.0.0.1 or on `::1`.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let start = Instant::now();
    let o = command(&exe).arg(port.to_string()).output().unwrap();
    let took = start.elapsed();
    let out = String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n");
    assert!(
        o.status.success(),
        "{out}{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert_eq!(out, "refused 15\n");
    assert!(took < LIMIT, "fifteen refused connects took {took:?}");
}
