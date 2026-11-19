//! A mapped file cut short while it is mapped (velt/fs `MappedFile`): reading a cut page is a bus
//! error, which ends the program. std's handler (std/fs/mapped_fault.velt) says what happened on
//! standard error first, and the program still ends by the signal (#3162). A `SIGBUS` another
//! process sends ends the program too, without the line, and a program that ignores `SIGBUS`
//! goes on ignoring it after it maps a file.

#![cfg(any(target_os = "linux", target_os = "macos"))]

mod no_window;
mod test_dir;

const SRC: &str = r#"
import * as fs from "velt/fs";
import { MappedFile } from "velt/fs";

function main() {
  const path = "cut.bin";
  fs.writeFileSync(path, "x".repeat(200000));
  const m = MappedFile.open(path);
  console.error("before", m.bytes[150000]);
  const fd = fs.openSync(path, "r+");
  fs.ftruncateSync(fd, 10);
  fs.closeSync(fd);
  console.error("cut", m.bytes[5]);
  const late = m.bytes[150000];
  console.error("unreachable", late);
}
"#;

#[test]
fn a_read_of_a_cut_page_says_so_and_ends_by_the_signal() {
    use std::os::unix::process::ExitStatusExt;
    let dir = test_dir::TestDir::new();
    std::fs::write(dir.path().join("main.velt"), SRC).unwrap();
    let out = crate::no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["run", "main.velt"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    let sigbus = if cfg!(target_os = "macos") { 10 } else { 7 };
    // The program's own process, or `velt run` passing its end on as an exit status.
    let by_signal = out.status.signal() == Some(sigbus) || out.status.code() == Some(128 + sigbus);
    assert!(by_signal, "{:?}\n{err}", out.status);
    // Standard error is not buffered: the lines before the fault, the handler's, nothing after.
    let before = err.find("before 120").unwrap_or_else(|| panic!("{err}"));
    let cut = err.find("cut 120").unwrap_or_else(|| panic!("{err}"));
    let line = err
        .find("velt: bus error: a page of a mapped file is not in the file any more")
        .unwrap_or_else(|| panic!("{err}"));
    assert!(before < cut && cut < line, "{err}");
    assert!(!err.contains("unreachable"), "{err}");
    assert_eq!(err.matches("velt: bus error").count(), 1, "{err}");
}

/// Maps a file, says `ready`, waits for a byte on standard input, says `done`.
const WAITS: &str = r#"
import * as fs from "velt/fs";
import { MappedFile } from "velt/fs";

function main() {
  fs.writeFileSync("kept.bin", "x".repeat(100000));
  const m = MappedFile.open("kept.bin");
  console.error("ready", m.bytes[0]);
  const one = Buffer.alloc(1);
  fs.readSync(0, one, 0, 1, null);
  console.error("done", m.bytes[1]);
}
"#;

/// Builds [`WAITS`], starts it (through `sh -c "<before> exec ./waits"`), sends it `SIGBUS` once
/// it has mapped its file, then gives it its byte: its exit status and standard error.
fn bus_signal_sent(before: &str) -> (std::process::ExitStatus, String) {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::process::Stdio;
    let dir = test_dir::TestDir::new();
    std::fs::write(dir.path().join("main.velt"), WAITS).unwrap();
    let built = crate::no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["build", "main.velt", "-o", "waits"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    let mut child = crate::no_window::command("sh")
        .args(["-c", &format!("{before} exec ./waits")])
        .current_dir(dir.path())
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut err = BufReader::new(child.stderr.take().unwrap());
    let mut text = String::new();
    // The file is mapped (and the handler installed) once it says so.
    err.read_line(&mut text).unwrap();
    assert!(text.starts_with("ready 120"), "{text}");
    let sent = crate::no_window::command("kill")
        .args(["-BUS", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(sent.success());
    // Its byte, if it is still there to read it.
    let _ = child.stdin.take().unwrap().write_all(b"x");
    err.read_to_string(&mut text).unwrap();
    (child.wait().unwrap(), text)
}

#[test]
fn a_bus_signal_another_process_sends_ends_the_program_without_the_line() {
    use std::os::unix::process::ExitStatusExt;
    let (status, err) = bus_signal_sent("");
    let sigbus = if cfg!(target_os = "macos") { 10 } else { 7 };
    assert_eq!(status.signal(), Some(sigbus), "{status:?}\n{err}");
    assert!(!err.contains("bus error") && !err.contains("done"), "{err}");
}

#[test]
fn a_program_that_ignores_the_bus_signal_still_does_after_mapping_a_file() {
    let (status, err) = bus_signal_sent("trap '' BUS;");
    assert_eq!(status.code(), Some(0), "{status:?}\n{err}");
    assert!(
        err.contains("done 120") && !err.contains("bus error"),
        "{err}"
    );
}
