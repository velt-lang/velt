//! Windows x64 only (docs/internals/design/unwinding.md "Windows"): the first raise of a process
//! installs the unhandled-exception filter that ends an unclaimed Velt raise with 101. When
//! several threads make their first raise at once, every one of them waits until that filter is
//! in place, so an unclaimed raise on any of them still ends the process with 101, never with the
//! exception's code (0xE0564C54). The probe module starts its threads with std's internal
//! `thread_win32.velt`, so std is copied with it added (as unwinding.rs does).
#![cfg(windows)]

use std::path::{Path, PathBuf};

mod no_window;
mod test_dir;

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            copy_tree(&path, &to.join(entry.file_name()));
        } else {
            std::fs::copy(&path, to.join(entry.file_name())).unwrap();
        }
    }
}

/// `velt/zz_race`: `n` threads that, released together, each panic with a catch counted on their
/// thread but no `velt_eh_try` frame under the panic, so each raise is the thread's first and is
/// unclaimed. The main thread waits for the process to end.
const PROBE: &str = r#"
import { Ptr, export as cExport } from "velt/ffi";
import { exportedAddress } from "velt/lang/intrinsics/address";
import * as th from "velt/lang/hooks/thread_win32";
import { enterCatch } from "velt/lang/hooks/unwind";

export { catchVeltPanic } from "velt/task/unwind";

declare c("c") function SwitchToThread(): i32;
declare c("c") function Sleep(ms: u32): void;

const READY = new Atomic<usize>(0);
const GO = new Atomic<u32>(0);

@cExport("probe_racer")
function racer(arg: Ptr<u8> | null): Ptr<u8> | null {
  READY.add(1);
  while (GO.load("acquire") === 0) {}
  if (enterCatch()) {
    panic("unclaimed");
  }
  return null;
}

/** Starts `n` racers, releases them together, and waits for the process to end. */
export function raceUnclaimed(n: usize): void {
  for (let i: usize = 0; i < n; i++) {
    // SAFETY: `racer` is a `void* f(void*)` that ignores its argument.
    th.start(unsafe { exportedAddress(racer) }, 0);
  }
  while (READY.load("acquire") < n) {
    // SAFETY: no precondition.
    unsafe {
      SwitchToThread();
    }
  }
  GO.store(1, "release");
  while (true) {
    // SAFETY: no precondition.
    unsafe {
      Sleep(1000);
    }
  }
}
"#;

const MAIN: &str = r#"
import { catchVeltPanic, raceUnclaimed } from "velt/zz_race";

function main() {
  // Catches nothing (so nothing is raised yet), but makes every panic of the program a raise.
  console.log(catchVeltPanic(() => {}));
  raceUnclaimed(8);
}
"#;

/// Fresh processes per build mode, each with 8 racing threads.
const RUNS: usize = 40;

#[test]
fn first_raises_racing_on_threads_all_exit_with_101() {
    let dir = test_dir::TestDir::new();
    let std: PathBuf = dir.path().join("std");
    copy_tree(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std"),
        &std,
    );
    std::fs::create_dir_all(std.join("zz_race")).unwrap();
    std::fs::write(std.join("zz_race/index.velt"), PROBE).unwrap();
    let file = dir.path().join("main.velt");
    std::fs::write(&file, MAIN).unwrap();
    for (mode, exe) in [
        (None, "race_debug.exe"),
        (Some("--release"), "race_release.exe"),
    ] {
        let mut args = vec!["build"];
        args.extend(mode);
        args.extend([file.to_str().unwrap(), "-o", exe]);
        let built = no_window::command(env!("CARGO_BIN_EXE_velt"))
            .args(&args)
            .env("VELT_STD", &std)
            .current_dir(dir.path())
            .output()
            .unwrap();
        assert!(
            built.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&built.stderr)
        );
        for run in 0..RUNS {
            let out = no_window::command(dir.path().join(exe))
                .current_dir(dir.path())
                .output()
                .unwrap();
            let stderr = String::from_utf8_lossy(&out.stderr);
            let why = format!("{exe}, run {run}: {:?}\n{stderr}", out.status);
            assert_eq!(out.status.code(), Some(101), "{why}");
            assert!(stderr.contains("panic: unclaimed"), "{why}");
        }
    }
}
