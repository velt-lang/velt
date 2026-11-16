//! The command description std/process/child passes to `execSync`, turned into a
//! `std::process::Command`.

use crate::str::VeltStr;
use crate::str_array::VeltStrArray;
use std::process::{Command, Stdio};

/// `CommandSpec` in std/process/child/spec.velt, field by field (size 104, align 8).
#[repr(C)]
pub struct VeltCommand {
    /// Program to run (looked up in `PATH` when it has no path separator).
    pub program: VeltStr,
    /// Arguments after the program name.
    pub args: VeltStrArray,
    /// Working directory; empty = the parent's.
    pub cwd: VeltStr,
    /// Extra environment variables as `name, value, name, value, …`.
    pub env: VeltStrArray,
    /// `stdin | stdout << 2 | stderr << 4`, each 0 = inherit, 1 = pipe, 2 = ignore (null);
    /// `execSync` pipes stdout and stderr whatever it says.
    pub stdio: u32,
    /// 1 = start from an empty environment (then only `env` is set).
    pub clear_env: u8,
}

const _: () = assert!(std::mem::size_of::<VeltCommand>() == 104);

/// How one standard stream of the child is connected.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StdioMode {
    /// A pipe the parent reads or writes.
    Pipe,
    /// Connected to the null device.
    Ignore,
}

impl StdioMode {
    fn stdio(self) -> Stdio {
        match self {
            StdioMode::Pipe => Stdio::piped(),
            StdioMode::Ignore => Stdio::null(),
        }
    }
}

unsafe fn text(s: &VeltStr) -> String {
    s.to_string_lossy()
}

unsafe fn strings(a: &VeltStrArray) -> Vec<String> {
    if a.len == 0 {
        return vec![];
    }
    std::slice::from_raw_parts(a.ptr, a.len as usize)
        .iter()
        .map(|s| text(s))
        .collect()
}

impl VeltCommand {
    /// A `Command` running the spec, its stdin, stdout and stderr connected as `modes` says.
    ///
    /// # Safety
    /// `self` must hold valid strings and string arrays (it is borrowed from Velt).
    pub unsafe fn build(&self, modes: [StdioMode; 3]) -> Command {
        let mut cmd = Command::new(text(&self.program));
        cmd.args(strings(&self.args));
        let cwd = text(&self.cwd);
        if !cwd.is_empty() {
            cmd.current_dir(cwd);
        }
        if self.clear_env != 0 {
            cmd.env_clear();
        }
        for [name, value] in strings(&self.env).as_chunks::<2>().0 {
            cmd.env(name, value);
        }
        cmd.stdin(modes[0].stdio())
            .stdout(modes[1].stdio())
            .stderr(modes[2].stdio());
        cmd
    }
}
