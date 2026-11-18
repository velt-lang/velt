//! A mapped file cut short while it is mapped (velt/fs `MappedFile`): reading a cut page is a bus
//! error, which ends the program. std's handler (std/fs/mapped_fault.velt) says what happened on
//! standard error first, and the program still ends by the signal (#3162).

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
