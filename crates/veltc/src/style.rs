//! Terminal colors for the CLI's own output (errors, status lines, help, test results). Colors are
//! used only when the stream is a terminal and `NO_COLOR` is unset or empty (no-color.org);
//! `TERM=dumb` disables them too. `CLICOLOR_FORCE=1` forces them on (e.g. for a pager).
//! On Windows the console's ANSI mode is switched on first; if that fails, no colors.

use std::io::IsTerminal;
use std::sync::OnceLock;

/// An output stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stream {
    /// Standard output.
    Stdout,
    /// Standard error.
    Stderr,
}

/// A text style.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    /// Bold red: `error:`, failures.
    Error,
    /// Bold yellow: `warning:`.
    Warning,
    /// Bold green: status verbs (`Created`), passing tests.
    Good,
    /// Bold: headings.
    Heading,
    /// Cyan: commands and flags in help text.
    Literal,
}

impl Style {
    fn ansi(self) -> &'static str {
        match self {
            Style::Error => "1;31",
            Style::Warning => "1;33",
            Style::Good => "1;32",
            Style::Heading => "1",
            Style::Literal => "36",
        }
    }
}

/// Whether output to `stream` is colored.
pub fn enabled(stream: Stream) -> bool {
    static STDOUT: OnceLock<bool> = OnceLock::new();
    static STDERR: OnceLock<bool> = OnceLock::new();
    let cell = match stream {
        Stream::Stdout => &STDOUT,
        Stream::Stderr => &STDERR,
    };
    *cell.get_or_init(|| {
        let tty = match stream {
            Stream::Stdout => std::io::stdout().is_terminal(),
            Stream::Stderr => std::io::stderr().is_terminal(),
        };
        let env = |name: &str| std::env::var_os(name).map(|v| v.to_string_lossy().into_owned());
        decide(tty, env("NO_COLOR"), env("CLICOLOR_FORCE"), env("TERM")) && console::enable(stream)
    })
}

/// The color decision from the environment (separate from I/O so it is testable).
fn decide(
    tty: bool,
    no_color: Option<String>,
    force: Option<String>,
    term: Option<String>,
) -> bool {
    if no_color.is_some_and(|v| !v.is_empty()) {
        return false;
    }
    if force.is_some_and(|v| !v.is_empty() && v != "0") {
        return true;
    }
    tty && term.as_deref() != Some("dumb")
}

/// `text` in `style` when `stream` is colored, else unchanged.
pub fn paint(stream: Stream, style: Style, text: &str) -> String {
    if enabled(stream) {
        format!("\x1b[{}m{text}\x1b[0m", style.ansi())
    } else {
        text.to_string()
    }
}

/// Print `error: <msg>` to stderr.
pub fn error(msg: &str) {
    eprintln!("{} {msg}", paint(Stream::Stderr, Style::Error, "error:"));
}

/// Print a cargo-style status line to stderr: the verb right-aligned in 12 columns, then `msg`.
pub fn status(verb: &str, msg: &str) {
    let verb = format!("{verb:>12}");
    eprintln!("{} {msg}", paint(Stream::Stderr, Style::Good, &verb));
}

#[cfg(windows)]
mod console {
    use super::Stream;
    use windows_sys::Win32::System::Console::{
        GetConsoleMode, GetStdHandle, SetConsoleMode, ENABLE_VIRTUAL_TERMINAL_PROCESSING,
        STD_ERROR_HANDLE, STD_OUTPUT_HANDLE,
    };

    /// Turn on ANSI escape processing for the console behind `stream`; false if impossible
    /// (not a console, or a Windows version without it).
    pub(super) fn enable(stream: Stream) -> bool {
        let which = match stream {
            Stream::Stdout => STD_OUTPUT_HANDLE,
            Stream::Stderr => STD_ERROR_HANDLE,
        };
        // SAFETY: plain Win32 calls on the process's own standard handle; `mode` is a valid
        // out-pointer for the duration of the call.
        unsafe {
            let handle = GetStdHandle(which);
            let mut mode = 0;
            if GetConsoleMode(handle, &mut mode) == 0 {
                // Not a console (e.g. a mintty pipe that is still a terminal): leave it be.
                return true;
            }
            mode & ENABLE_VIRTUAL_TERMINAL_PROCESSING != 0
                || SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING) != 0
        }
    }
}

#[cfg(not(windows))]
mod console {
    /// ANSI escapes work on every Unix terminal.
    pub(super) fn enable(_stream: super::Stream) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> Option<String> {
        Some(v.to_string())
    }

    #[test]
    fn color_decision() {
        assert!(decide(true, None, None, s("xterm-256color")));
        assert!(!decide(false, None, None, None), "not a terminal");
        assert!(!decide(true, s("1"), None, None), "NO_COLOR");
        assert!(decide(true, s(""), None, None), "empty NO_COLOR is ignored");
        assert!(!decide(true, None, None, s("dumb")));
        assert!(decide(false, None, s("1"), None), "CLICOLOR_FORCE");
        assert!(!decide(false, None, s("0"), None));
        assert!(!decide(false, s("1"), s("1"), None), "NO_COLOR wins");
    }
}
