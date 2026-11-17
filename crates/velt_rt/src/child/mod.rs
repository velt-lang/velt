//! `execSync` in std/process/child: running a program to completion on the calling thread
//! (`output.rs`), from the command std describes (`command.rs`). Starting a child with
//! `spawn` and `exec` is std's own (std/process/child/child_sched.velt).

mod command;
mod output;

// The tests spawn unix tools (sh, cat); Windows is covered by the goldens.
#[cfg(all(test, unix))]
mod tests;
