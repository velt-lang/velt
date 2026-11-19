//! Reading whole files whose size `fstat` doesn't tell (std/fs/whole/): a FIFO, whose size reads 0,
//! comes back whole, read to its end; and a file another process appends to while the program
//! reads it again and again gives, each time, what was there: at least what was there at the start,
//! and only the bytes written, never what an array sized for an older size held.

use std::io::Write;
use std::path::Path;
use std::process::Stdio;

use no_window::command;

mod no_window;
mod runtime_support;
mod test_dir;

/// Reads the file named by its argument `rounds` times (the second argument), and prints the
/// shortest and longest length, and whether every byte was `fill` (the third, a char code).
const PROGRAM: &str = r#"import { readFileBytesSync } from "velt/fs";
import { argv } from "velt/process";

function main() {
  const args = argv();
  const path = args[1];
  const rounds = parseInt(args[2]);
  const fill = parseInt(args[3]);
  let shortest = -1;
  let longest = 0;
  let only = true;
  for (let i = 0; i < rounds; i++) {
    const bytes = readFileBytesSync(path);
    const n = bytes.length as number;
    shortest = shortest < 0 || n < shortest ? n : shortest;
    longest = n > longest ? n : longest;
    for (let j: usize = 0; j < bytes.length; j++) {
      if ((bytes[j] as number) != fill) {
        only = false;
      }
    }
  }
  console.log(shortest, longest, only);
}
"#;

fn build(dir: &Path) -> std::path::PathBuf {
    runtime_support::build_native_runtime(Path::new(env!("CARGO_MANIFEST_DIR")));
    let src = dir.join("reads.velt");
    std::fs::write(&src, PROGRAM).unwrap();
    let exe = dir.join(if cfg!(windows) { "reads.exe" } else { "reads" });
    let built = command(env!("CARGO_BIN_EXE_velt"))
        .arg("build")
        .arg(&src)
        .arg("-o")
        .arg(&exe)
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    exe
}

/// What the program prints for `path`, `rounds` reads, every byte `fill`.
fn reads(exe: &Path, path: &Path, rounds: u32, fill: u8) -> std::process::Child {
    command(exe)
        .arg(path)
        .arg(rounds.to_string())
        .arg(fill.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

fn printed(child: std::process::Child) -> String {
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[cfg(unix)]
#[test]
fn a_fifo_is_read_to_its_end() {
    let dir = test_dir::TestDir::new();
    let exe = build(dir.path());
    let fifo = dir.path().join("fifo");
    let made = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap();
    assert!(made.success());
    let child = reads(&exe, &fifo, 1, b'y');
    // Opening for writing waits for the reader's open. On a thread, so that a program that fails
    // before it opens the FIFO fails the test instead of leaving it waiting.
    let path = fifo.clone();
    std::thread::spawn(move || {
        let mut w = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        for _ in 0..50 {
            w.write_all(&[b'y'; 4000]).unwrap();
        }
    });
    assert_eq!(printed(child), "200000 200000 true");
}

#[test]
fn a_growing_file_reads_as_what_was_there() {
    let dir = test_dir::TestDir::new();
    let exe = build(dir.path());
    let file = dir.path().join("growing.txt");
    std::fs::write(&file, [b'z'; 100_000]).unwrap();
    let child = reads(&exe, &file, 200, b'z');
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&file)
        .unwrap();
    // Append while the program reads, until it has printed (its stdout closes as it exits).
    let mut appended = 0;
    while appended < 2_000 {
        f.write_all(&[b'z'; 1000]).unwrap();
        appended += 1;
    }
    let line = printed(child);
    let parts: Vec<&str> = line.split(' ').collect();
    assert_eq!(parts.len(), 3, "{line}");
    let shortest: u64 = parts[0].parse().unwrap();
    let longest: u64 = parts[1].parse().unwrap();
    assert!(shortest >= 100_000 && longest >= shortest, "{line}");
    assert!(longest <= 100_000 + 2_000 * 1000, "{line}");
    assert_eq!(parts[2], "true", "{line}");
}
