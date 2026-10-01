//! The command description std/child_process passes to the runtime, turned into a
//! `std::process::Command` (shared by `spawn`, `output` and `output_sync`).

use crate::str::VeltStr;
use crate::str_array::VeltStrArray;
use std::process::{Command, Stdio};

/// `CommandSpec` in std/child_process.vlt, field by field (size 104, align 8).
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
    /// `stdin | stdout << 2 | stderr << 4`, each 0 = inherit, 1 = pipe, 2 = ignore (null).
    pub stdio: u32,
    /// 1 = start from an empty environment (then only `env` is set).
    pub clear_env: u8,
}

const _: () = assert!(std::mem::size_of::<VeltCommand>() == 104);

/// How one standard stream of the child is connected.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StdioMode {
    /// Shares the parent's stream.
    Inherit,
    /// A pipe the parent reads or writes.
    Pipe,
    /// Connected to the null device.
    Ignore,
}

impl StdioMode {
    fn from_bits(bits: u32) -> StdioMode {
        match bits & 3 {
            1 => StdioMode::Pipe,
            2 => StdioMode::Ignore,
            _ => StdioMode::Inherit,
        }
    }

    fn stdio(self) -> Stdio {
        match self {
            StdioMode::Inherit => Stdio::inherit(),
            StdioMode::Pipe => Stdio::piped(),
            StdioMode::Ignore => Stdio::null(),
        }
    }
}

unsafe fn text(s: &VeltStr) -> String {
    String::from_utf8_lossy(s.as_bytes()).into_owned()
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
    /// The stdio modes of stdin, stdout and stderr.
    pub fn modes(&self) -> [StdioMode; 3] {
        [0, 2, 4].map(|shift| StdioMode::from_bits(self.stdio >> shift))
    }

    /// A `Command` with everything but stdio configured; `modes` overrides the spec's stdio.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stdio_bits() {
        let spec = VeltCommand {
            program: VeltStr::from_static(b"x"),
            args: VeltStrArray::from_vec(vec![]),
            cwd: VeltStr::empty(),
            env: VeltStrArray::from_vec(vec![]),
            stdio: 1 | (2 << 2),
            clear_env: 0,
        };
        assert_eq!(
            spec.modes(),
            [StdioMode::Pipe, StdioMode::Ignore, StdioMode::Inherit]
        );
    }
}
