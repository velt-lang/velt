//! `Date`'s local getters follow `TZ`, as in Node: UTC, a fixed offset (`Etc/GMT-2`, whose sign
//! is inverted by convention), and on macOS and Linux a region with daylight saving time
//! (`Europe/Berlin`, on both sides of its 2026 change), plus an instant before the epoch, which
//! rounds down to its second. The program is built once and run under each `TZ` (the C library
//! reads it once per process). Compared with `node` when it is installed.
//!
//! On Windows the C library reads no `TZ`: Velt understands the UTC and fixed-offset names there
//! (std/time/tz_fixed.velt) and keeps the system zone for a region name, so the region runs on
//! macOS and Linux only.

use std::path::Path;
use std::process::Command;

use no_window::command;

mod no_window;
mod runtime_support;
mod test_dir;

const PROGRAM: &str = r#"function main() {
  const instants = [
    Date.UTC(2026, 0, 15, 12),
    Date.UTC(2026, 6, 15, 12),
    Date.UTC(2026, 2, 29, 0, 59, 59),
    Date.UTC(2026, 2, 29, 1),
    -500,
  ];
  for (const t of instants) {
    const d = new Date(t);
    console.log(d.getTimezoneOffset(), d.getHours(), d.getDate());
  }
}
"#;

/// `TZ` and what the program prints under it (Node 24's output).
const ZONES: &[(&str, &str)] = &[
    ("UTC", "0 12 15\n0 12 15\n0 0 29\n0 1 29\n0 23 31\n"),
    (
        "Etc/GMT-2",
        "-120 14 15\n-120 14 15\n-120 2 29\n-120 3 29\n-120 1 1\n",
    ),
    (
        "Europe/Berlin",
        "-60 13 15\n-120 14 15\n-60 1 29\n-120 3 29\n-60 0 1\n",
    ),
];

fn stdout(o: &std::process::Output) -> String {
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n")
}

fn velt() -> Command {
    command(env!("CARGO_BIN_EXE_velt"))
}

#[test]
fn local_getters_follow_tz_as_node() {
    runtime_support::build_native_runtime(Path::new(env!("CARGO_MANIFEST_DIR")));
    let dir = test_dir::TestDir::new();
    let src = dir.path().join("zones.velt");
    std::fs::write(&src, PROGRAM).unwrap();
    let exe = dir
        .path()
        .join(if cfg!(windows) { "zones.exe" } else { "zones" });
    stdout(
        &velt()
            .arg("build")
            .arg(&src)
            .arg("-o")
            .arg(&exe)
            .output()
            .unwrap(),
    );
    let ts = dir.path().join("zones.ts");
    std::fs::write(&ts, format!("{PROGRAM}main();\n")).unwrap();
    for (tz, want) in ZONES {
        if cfg!(windows) && tz.contains('/') && !tz.starts_with("Etc/") {
            continue;
        }
        let got = stdout(&command(&exe).env("TZ", tz).output().unwrap());
        assert_eq!(got, *want, "TZ={tz}");
        if let Ok(node) = command("node")
            .arg("--experimental-strip-types")
            .arg(&ts)
            .env("TZ", tz)
            .output()
        {
            if node.status.success() {
                assert_eq!(stdout(&node), *want, "node, TZ={tz}");
            }
        }
    }
}
