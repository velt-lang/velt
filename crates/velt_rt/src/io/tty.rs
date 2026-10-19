//! `isatty(fd)` and `process.stdout.isTTY` (std/process.vlt): is a file descriptor a terminal?
//!
//! The answer for the standard streams is computed once and cached, as Node fixes `isTTY` when it
//! creates `process.stdout`: a CLI may ask on every write (to colour its output) for the price of
//! a load.

use std::io::IsTerminal;
use std::sync::atomic::{AtomicU8, Ordering};

/// Per standard stream (0, 1, 2): 0 not asked yet, 1 not a terminal, 2 a terminal.
static STD: [AtomicU8; 3] = [AtomicU8::new(0), AtomicU8::new(0), AtomicU8::new(0)];

/// 1 if `fd` is open and refers to a terminal, else 0 (Node's `tty.isatty(fd)`). On Windows only
/// the standard streams (0, 1, 2) can be terminals: other numbers are no handles there.
#[no_mangle]
pub extern "C" fn velt_rt_isatty(fd: i32) -> u8 {
    let Some(slot) = usize::try_from(fd).ok().and_then(|i| STD.get(i)) else {
        return probe(fd) as u8;
    };
    match slot.load(Ordering::Relaxed) {
        0 => {
            let tty = probe(fd);
            slot.store(if tty { 2 } else { 1 }, Ordering::Relaxed);
            tty as u8
        }
        state => (state == 2) as u8,
    }
}

fn probe(fd: i32) -> bool {
    match fd {
        0 => std::io::stdin().is_terminal(),
        1 => std::io::stdout().is_terminal(),
        2 => std::io::stderr().is_terminal(),
        _ => other_fd(fd),
    }
}

#[cfg(unix)]
fn other_fd(fd: i32) -> bool {
    // SAFETY: isatty only inspects the descriptor; a closed or negative one is just "no".
    unsafe { libc::isatty(fd) == 1 }
}

#[cfg(not(unix))]
fn other_fd(_fd: i32) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closed_and_negative_descriptors_are_not_terminals() {
        assert_eq!(velt_rt_isatty(-1), 0);
        assert_eq!(velt_rt_isatty(9999), 0);
    }

    #[test]
    fn standard_streams_are_cached() {
        let first = velt_rt_isatty(1);
        assert_eq!(velt_rt_isatty(1), first);
        assert_ne!(STD[1].load(Ordering::Relaxed), 0);
    }
}
