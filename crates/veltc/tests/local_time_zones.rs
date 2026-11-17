//! `Date`'s local getters follow `TZ`, as in Node: UTC, a fixed offset (`Etc/GMT-2`, whose sign
//! is inverted by convention), and on macOS and Linux a region with daylight saving time
//! (`Europe/Berlin`, on both sides of its 2026 change), plus an instant before the epoch, which
//! rounds down to its second. Local times in a gap and an overlap (Berlin, New York, Lord Howe's
//! 30-minute change) pick Node's instant, and a local mean time keeps its seconds (Kolkata, 1900).
//! The program is built once and run under each `TZ` (the C library reads it once per process).
//! Compared with `node` when it is installed.
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
  // Local times in a gap (the clock set forward) and an overlap (set back), and a local mean time
  // with seconds in its offset (1900).
  const locals = [
    new Date(2026, 2, 8, 2, 30),
    new Date(2026, 9, 25, 2, 30),
    new Date(2026, 3, 5, 1, 45),
    new Date(1901, 0, 1),
    new Date("2026-03-08T02:30:00"),
  ];
  for (const d of locals) {
    console.log(d.getTime(), d.getTimezoneOffset(), d.getHours(), d.getMinutes());
  }
  const lmt = new Date(Date.UTC(1900, 0, 1));
  console.log(lmt.getTimezoneOffset(), lmt.getHours(), lmt.getMinutes(), lmt.getSeconds());
}
"#;

/// `TZ` and what the program prints under it (Node 24's output).
const ZONES: &[(&str, &str)] = &[
    (
        "UTC",
        "0 12 15\n0 12 15\n0 0 29\n0 1 29\n0 23 31\n\
         1772937000000 0 2 30\n1792895400000 0 2 30\n1775353500000 0 1 45\n\
         -2177452800000 0 0 0\n1772937000000 0 2 30\n0 0 0 0\n",
    ),
    (
        "Etc/GMT-2",
        "-120 14 15\n-120 14 15\n-120 2 29\n-120 3 29\n-120 1 1\n\
         1772929800000 -120 2 30\n1792888200000 -120 2 30\n1775346300000 -120 1 45\n\
         -2177460000000 -120 0 0\n1772929800000 -120 2 30\n-120 2 0 0\n",
    ),
    // The overlap of 2026-10-25: the first 02:30 (CEST), as Node.
    (
        "Europe/Berlin",
        "-60 13 15\n-120 14 15\n-60 1 29\n-120 3 29\n-60 0 1\n\
         1772933400000 -60 2 30\n1792888200000 -120 2 30\n1775346300000 -120 1 45\n\
         -2177456400000 -60 0 0\n1772933400000 -60 2 30\n-60 1 0 0\n",
    ),
    // The gap of 2026-03-08: 02:30 is 03:30 EDT, as Node.
    (
        "America/New_York",
        "300 7 15\n240 8 15\n240 20 28\n240 21 28\n300 18 31\n\
         1772955000000 240 3 30\n1792909800000 240 2 30\n1775367900000 240 1 45\n\
         -2177434800000 300 0 0\n1772955000000 240 3 30\n300 19 0 0\n",
    ),
    // Local mean time until 1901 (+5:21:10): seconds in the fields, minutes truncated in
    // `getTimezoneOffset()`, as V8.
    (
        "Asia/Kolkata",
        "-330 17 15\n-330 17 15\n-330 6 29\n-330 6 29\n-330 5 1\n\
         1772917200000 -330 2 30\n1792875600000 -330 2 30\n1775333700000 -330 1 45\n\
         -2177472070000 -321 0 0\n1772917200000 -330 2 30\n-321 5 21 10\n",
    ),
    // A 30-minute change, and its overlap on 2026-04-05: the first 01:45 (+11), as Node.
    (
        "Australia/Lord_Howe",
        "-660 23 15\n-630 22 15\n-660 11 29\n-660 12 29\n-600 9 1\n\
         1772897400000 -660 2 30\n1792855800000 -660 2 30\n1775313900000 -660 1 45\n\
         -2177488800000 -600 0 0\n1772897400000 -660 2 30\n-600 10 0 0\n",
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
