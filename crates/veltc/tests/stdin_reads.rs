//! Standard input read through std's buffer (std/process/stdin_buffer.velt): a program reads two
//! lines with `readLineSync`, then the rest with `readAllSync`, and prints what it got, for inputs
//! piped in whole. Lines end at "\n", a "\r" before it is dropped, the last line needs no "\n",
//! invalid UTF-8 reads as U+FFFD (as Node's `fs.readFileSync(0, "utf8")`), a line longer than one
//! 64 KiB refill comes back whole, and the end of input is `null`, after which `readAllSync` is
//! "".

use std::io::Write;
use std::path::Path;
use std::process::Stdio;

use no_window::command;

mod no_window;
mod runtime_support;
mod test_dir;
#[allow(dead_code)]
mod wasm_tools;

const PROGRAM: &str = r#"import { readAllSync, readLineSync } from "velt/process";

function show(s: string | null): string {
  if (s === null) {
    return "null";
  }
  return s.length > 40 ? `<${s.length} chars, ends ${JSON.stringify(s.slice(s.length - 3))}>` : JSON.stringify(s);
}

function main() {
  console.log(show(readLineSync()));
  console.log(show(readLineSync()));
  console.log(show(readAllSync()));
  console.log(show(readLineSync()));
  console.log(show(readAllSync()));
}
"#;

/// Four tasks on the executor's threads read lines at once until the end of input. Each line is
/// read by exactly one of them, so the count and the sum of the numbers are the input's, and each
/// reader's numbers ascend.
const PARALLEL: &str = r#"import { readLineSync } from "velt/process";

async function reader(): Promise<i64[]> {
  let count: i64 = 0;
  let sum: i64 = 0;
  let last: i64 = 0;
  let unordered: i64 = 0;
  let line = readLineSync();
  while (line !== null) {
    const n = parseInt(line) as i64;
    count++;
    sum += n;
    if (n <= last) {
      unordered++;
    }
    last = n;
    line = readLineSync();
  }
  return [count, sum, unordered];
}

async function main() {
  const ps: Promise<i64[]>[] = [];
  for (let t = 0; t < 4; t++) {
    ps.push(spawn(reader()));
  }
  let count: i64 = 0;
  let sum: i64 = 0;
  let unordered: i64 = 0;
  for (const r of await Promise.all(ps)) {
    count += r[0];
    sum += r[1];
    unordered += r[2];
  }
  console.log(count, sum, unordered);
}
"#;

/// `src` built into an executable in `dir`.
fn build(dir: &Path, name: &str, src: &str) -> std::path::PathBuf {
    runtime_support::build_native_runtime(Path::new(env!("CARGO_MANIFEST_DIR")));
    let file = dir.join(format!("{name}.velt"));
    std::fs::write(&file, src).unwrap();
    let exe = dir.join(if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    });
    let built = command(env!("CARGO_BIN_EXE_velt"))
        .arg("build")
        .arg(&file)
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

fn run(exe: &Path, input: &[u8]) -> String {
    run_command(command(exe), input)
}

/// What `cmd` prints with `input` piped in; it must succeed.
fn run_command(mut cmd: std::process::Command, input: &[u8]) -> String {
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n")
}

/// PROGRAM's inputs and what it prints for each.
fn cases() -> Vec<(Vec<u8>, &'static str)> {
    let long = format!("{}xyz\nnext\n", "a".repeat(70_000));
    vec![
        (
            b"one\r\ntwo\nrest\r\nof it".to_vec(),
            "\"one\"\n\"two\"\n\"rest\\r\\nof it\"\nnull\n\"\"\n",
        ),
        (b"".to_vec(), "null\nnull\n\"\"\nnull\n\"\"\n"),
        (b"only".to_vec(), "\"only\"\nnull\n\"\"\nnull\n\"\"\n"),
        (b"\n\n".to_vec(), "\"\"\n\"\"\n\"\"\nnull\n\"\"\n"),
        (
            b"c\xffd\nx\n".to_vec(),
            "\"c\u{fffd}d\"\n\"x\"\n\"\"\nnull\n\"\"\n",
        ),
        (
            long.into_bytes(),
            "<70003 chars, ends \"xyz\">\n\"next\"\n\"\"\nnull\n\"\"\n",
        ),
    ]
}

fn check_cases(mut run_one: impl FnMut(&[u8]) -> String) {
    for (input, want) in cases() {
        assert_eq!(
            run_one(&input),
            want,
            "input {:?}",
            String::from_utf8_lossy(&input[..input.len().min(40)])
        );
    }
}

#[test]
fn lines_and_the_rest_of_standard_input() {
    let dir = test_dir::TestDir::new();
    let exe = build(dir.path(), "stdin_reads", PROGRAM);
    check_cases(|input| run(&exe, input));
}

/// The same reads on wasm32-wasip1 (`read` on fd 0 through wasi-libc), under wasmtime.
#[test]
fn lines_and_the_rest_of_standard_input_on_wasi() {
    if let Some(missing) = wasm_tools::missing_tools("wasm32-wasip1", "wasmtime") {
        eprintln!("note: {missing} not available; skipping standard input on WASI");
        return;
    }
    wasm_tools::build_runtime(&wasm_tools::root(), "wasm32-wasip1");
    // Its own directory: `velt run` of a lone file builds below the current directory.
    let dir = test_dir::TestDir::new();
    let src = dir.path().join("stdin_reads.velt");
    std::fs::write(&src, PROGRAM).unwrap();
    check_cases(|input| {
        let mut cmd = command(env!("CARGO_BIN_EXE_velt"));
        cmd.args(["run", "--target", "wasm32-wasip1"])
            .arg(&src)
            .current_dir(dir.path());
        run_command(cmd, input)
    });
}

/// The buffer's lock: readers on several threads share one stream, each line read once.
#[test]
fn readers_on_several_threads_share_the_stream() {
    let dir = test_dir::TestDir::new();
    let exe = build(dir.path(), "stdin_parallel", PARALLEL);
    let n: u64 = 200_000;
    let input: String = (1..=n).map(|i| format!("{i}\n")).collect();
    for _ in 0..3 {
        assert_eq!(
            run(&exe, input.as_bytes()),
            format!("{} {} 0\n", n, n * (n + 1) / 2)
        );
    }
}
