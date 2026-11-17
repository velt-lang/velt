//! std's own C declarations against the C library's headers (`velt native headers --std`): std
//! has no package.velt, so the headers are listed here.

use std::collections::HashSet;
use std::path::Path;
use std::sync::OnceLock;

use velt_common::{Diagnostics, FileId, SourceMap};
use velt_sema::hir;

use super::{check_library, decl_of, zig_target, LibraryHeaders};

/// The C library headers std's `declare c("c")` functions are checked against (std has no
/// package.velt to list them in), per target: `velt native headers --std` reads them. Windows
/// targets load none of std's POSIX declarations (the `#fd-system` variant there declares
/// nothing), only the C library's memory and string functions and `errno`'s location
/// (velt/ffi's `#errno-location`); `<unwind.h>` has the Itanium unwinder's
/// `_Unwind_RaiseException` (std/lang/hooks/unwind_itanium.velt); the socket headers
/// (`<sys/socket.h>`, `<netdb.h>`, `<arpa/inet.h>`, and `<strings.h>` for `bcopy`) have velt/net's
/// calls (std/net/calls_posix.velt).
fn std_headers(target: &str) -> Vec<&'static str> {
    if target.contains("-windows") {
        return vec!["<errno.h>", "<stdlib.h>", "<string.h>", "<windows.h>"];
    }
    let mut headers = vec![
        "<arpa/inet.h>",
        "<errno.h>",
        "<fcntl.h>",
        "<netdb.h>",
        "<pthread.h>",
        "<sched.h>",
        "<signal.h>",
        "<spawn.h>",
        "<stdlib.h>",
        "<string.h>",
        "<strings.h>",
        "<sys/socket.h>",
        "<sys/stat.h>",
        "<sys/wait.h>",
        "<time.h>",
        "<unistd.h>",
        "<unwind.h>",
    ];
    // The reactors' (std/lang/hooks/reactor_kqueue.velt, reactor_epoll.velt), and on macOS
    // velt/os' (`sysctlbyname`, std/os/sys_darwin.velt) and velt/process' (`_NSGetEnviron`,
    // std/process/env_darwin.velt).
    if target.contains("-apple-") {
        headers.extend([
            "<crt_externs.h>",
            "<mach/mach.h>",
            "<sys/event.h>",
            "<sys/sysctl.h>",
        ]);
    } else if target.contains("-linux") {
        headers.extend(["<sys/epoll.h>", "<sys/eventfd.h>"]);
    }
    headers
}

/// The macros std's C calls are declared with on `target`: Linux's C libraries declare
/// `accept4` and `pipe2` (the epoll reactor's, std/lang/hooks/reactor_epoll.velt) and
/// `sched_getaffinity` (velt/os', std/os/sys_linux.velt) with `_GNU_SOURCE` only.
fn std_defines(target: &str) -> Vec<(String, String)> {
    match target.contains("-linux") {
        true => vec![("_GNU_SOURCE".into(), "1".into())],
        false => Vec::new(),
    }
}

/// The std files whose `declare c("c")` functions are checked, relative to std's root: those
/// whose declarations match their headers today. The rest of std's declarations (UDP sockets and
/// `getaddrinfo`, the reactors, std's calls of the runtime) differ from their headers in ways
/// #3022 lists, and are brought under the check file by file there. This list only grows: a file
/// leaves it only when it no longer declares C functions.
const CHECKED: &[&str] = &[
    "ffi/errno_darwin.velt",
    "ffi/errno_linux.velt",
    "ffi/errno_windows.velt",
    "ffi/read.velt",
    "fs/fd/posix.velt",
    "lang/hooks/os_posix.velt",
    "lang/hooks/pid_posix.velt",
    "lang/hooks/pid_win32.velt",
    "lang/hooks/reactor_epoll.velt",
    "lang/hooks/reactor_kqueue.velt",
    "lang/hooks/sched_cells.velt",
    "lang/hooks/sched_end.velt",
    "lang/hooks/sched_exit.velt",
    "lang/hooks/sched_id.velt",
    "lang/hooks/sched_io_write.velt",
    "lang/hooks/sched_mem.velt",
    "lang/hooks/sched_module.velt",
    "lang/hooks/sched_node_loc.velt",
    "lang/hooks/sched_off.velt",
    "lang/hooks/sched_peek.velt",
    "lang/hooks/sched_read.velt",
    "lang/hooks/sched_wake.velt",
    "lang/hooks/sched_workers.velt",
    "lang/hooks/thread_posix.velt",
    "lang/hooks/unwind_itanium.velt",
    "net/calls_posix.velt",
    "net/fd_io.velt",
    "net/fd_until.velt",
    "net/rt_later_flush.velt",
    "net/udp/udp_sched.velt",
    "os/posix.velt",
    "os/sys_darwin.velt",
    "os/sys_linux.velt",
    "process/child/child_win32.velt",
    "process/child/command_line_win32.velt",
    "process/child/exit_win32.velt",
    "process/child/handle_list_win32.velt",
    "process/child/pipes_win32.velt",
    "process/child/spawn_win32.velt",
    "process/cwd_posix.velt",
    "process/cwd_win32.velt",
    "process/env_darwin.velt",
    "process/env_linux.velt",
    "process/env_posix.velt",
    "process/env_win32.velt",
    "process/tty_posix.velt",
    "sync/output.velt",
    "sync/slot.velt",
];

/// The std files [`CHECKED`] would list but that only macOS and Linux builds reach: child
/// processes on the executor's reactor (std/process/child/child_posix.velt, which the hook module
/// child_reactor.velt loads through `#child-os` everywhere but on Windows, and the files it
/// imports), so their POSIX calls are checked against macOS's and Linux's headers only. Sorted,
/// each file once, as `CHECKED`.
const CHECKED_POSIX: &[&str] = &[
    "process/child/child_posix.velt",
    "process/child/child_record.velt",
    "process/child/spawn_open_darwin.velt",
    "process/child/spawn_open_linux.velt",
    "process/child/spawn_posix.velt",
];

/// The symbols the runtime (velt_rt) exports: velt_common's tests/runtime_exports.txt, which its
/// test `the_runtime_exports_only_what_runtime_exports_lists` keeps equal to the built runtime's
/// exports. std calls them through `declare c("c")` (the executor in Velt's hooks reaching the
/// runtime), and no C header declares them, so the check skips exactly these: by membership,
/// not by a `velt_rt_` prefix, as the runtime moves into std (#2196) and a symbol it no longer
/// exports is checked like any other (#3022).
fn runtime_exports() -> &'static HashSet<&'static str> {
    static EXPORTS: OnceLock<HashSet<&'static str>> = OnceLock::new();
    EXPORTS.get_or_init(|| {
        include_str!("../../../../velt_common/tests/runtime_exports.txt")
            .lines()
            .filter_map(|l| l.strip_prefix("velt_rt "))
            .map(str::trim)
            .collect()
    })
}

/// C functions std calls that no public header declares, each with why std calls it anyway. The
/// check skips exactly these names. Keep it short: an entry is a function std needs and the
/// platform ships, but only in a private header.
const NO_PUBLIC_HEADER: &[(&str, &str)] = &[
    // macOS's futex (std/lang/hooks/reactor_kqueue.velt's park and wake): in libSystem, declared
    // only by xnu's bsd/sys/ulock.h, which the SDK doesn't ship. libc++ and Rust's std call it
    // the same way.
    (
        "__ulock_wait",
        "macOS's futex wait, declared in xnu's bsd/sys/ulock.h only",
    ),
    (
        "__ulock_wake",
        "macOS's futex wake, declared in xnu's bsd/sys/ulock.h only",
    ),
];

/// `file` (std at `root`) is one [`CHECKED`] lists, or, on a target other than Windows (`windows`
/// false), one [`CHECKED_POSIX`] lists.
fn is_checked(sm: &SourceMap, root: &Path, file: FileId, windows: bool) -> bool {
    let path = &sm.get(file).path;
    // The loader's path and `root` may reach std differently (a symlinked temporary directory).
    let rel = match path.strip_prefix(root) {
        Ok(rel) => rel.to_path_buf(),
        Err(_) => match (path.canonicalize(), root.canonicalize()) {
            (Ok(path), Ok(root)) => match path.strip_prefix(&root) {
                Ok(rel) => rel.to_path_buf(),
                Err(_) => return false,
            },
            _ => return false,
        },
    };
    let rel = rel.to_string_lossy().replace('\\', "/");
    CHECKED.contains(&rel.as_str()) || (!windows && CHECKED_POSIX.contains(&rel.as_str()))
}

/// Check std's own C declarations (those in `std_files` that [`CHECKED`] lists, and on a target
/// other than Windows [`CHECKED_POSIX`]; std at `root`) against the C library's headers
/// ([`std_headers`]) for `target`, as a package's are: `velt native headers --std`, with zig.
/// Builds take them as checked. Nothing is remembered for a record: std has none.
pub fn check_std(
    p: &hir::Program,
    sm: &SourceMap,
    root: &Path,
    std_files: &HashSet<FileId>,
    target: &str,
    diags: &mut Diagnostics,
) {
    let mut decls = vec![];
    for def in &p.defs {
        match decl_of(p, def) {
            Some(("c", d))
                if std_files.contains(&d.span.file)
                    && !runtime_exports().contains(d.name.as_str())
                    && !NO_PUBLIC_HEADER.iter().any(|(name, _)| *name == d.name)
                    && is_checked(sm, root, d.span.file, target.contains("-windows")) =>
            {
                decls.push(d)
            }
            _ => {}
        }
    }
    let Some(zig_target) = zig_target(target) else {
        return;
    };
    if decls.is_empty() {
        return;
    }
    let headers: Vec<String> = std_headers(target).iter().map(|h| h.to_string()).collect();
    let read = LibraryHeaders {
        headers: &headers,
        defines: &std_defines(target),
    };
    check_library(root, "c", read, &decls, (zig_target, target), diags);
}

#[cfg(test)]
mod tests {
    use super::{runtime_exports, CHECKED, CHECKED_POSIX};

    /// The skip is by membership: a symbol the runtime exports is skipped, one with its prefix
    /// that it doesn't export is checked, as is a C library function.
    #[test]
    fn only_the_runtime_s_exports_are_skipped() {
        let exports = runtime_exports();
        assert!(exports.contains("velt_rt_timer_new"));
        assert!(exports.contains("velt_rt_sleep"));
        assert!(!exports.contains("velt_rt_not_exported"));
        assert!(!exports.contains("strlen"));
        // The WebAssembly runtime's lines are not velt_rt's.
        assert!(!exports.contains("velt_host_alloc"));
    }

    /// A renamed or moved file would drop out of the check without a word: each one listed is
    /// in std and declares C functions, and each list is sorted, each file once, in one list.
    #[test]
    fn every_checked_file_is_in_std_and_declares_c_functions() {
        let std = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std");
        for file in CHECKED.iter().chain(CHECKED_POSIX) {
            let text = std::fs::read_to_string(std.join(file)).unwrap_or_else(|e| {
                panic!("std/{file}, which CHECKED or CHECKED_POSIX lists: {e}")
            });
            assert!(
                text.contains("declare c(\"c\")"),
                "std/{file} declares no C functions: take it out of CHECKED"
            );
        }
        let mut sorted = CHECKED.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted, CHECKED, "CHECKED is sorted, each file once");
        let mut sorted = CHECKED_POSIX.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted, CHECKED_POSIX,
            "CHECKED_POSIX is sorted, each file once"
        );
        for file in CHECKED_POSIX {
            assert!(!CHECKED.contains(file), "std/{file} is in both lists");
        }
    }
}
