//! `execSync` in std/process/child on Windows and WebAssembly only: running a program to
//! completion on the calling thread (`output.rs`), from the command std describes
//! (`command.rs`). macOS and Linux start it in std (std/process/child/exec_sync_posix.velt), and
//! starting a child with `spawn` and `exec` is std's own everywhere
//! (std/process/child/child_sched.velt). Windows' path is covered by the goldens.

mod command;
mod output;
