//! std's own C declarations against the C library's headers (`velt native headers --std`): std
//! has no package.velt, so the headers are listed here.

use std::collections::HashSet;
use std::path::Path;

use velt_common::{Diagnostics, FileId};
use velt_sema::hir;

use super::{check_library, decl_of, zig_target, LibraryHeaders};

/// The C library headers std's `declare c("c")` functions are checked against (std has no
/// package.velt to list them in), per target: `velt native headers --std` reads them. Windows
/// targets load none of std's POSIX declarations (the `#fd-system` variant there declares
/// nothing), only the C library's memory and string functions and `errno`'s location
/// (velt/ffi's `#errno-location`); `<unwind.h>` has the Itanium unwinder's
/// `_Unwind_RaiseException` (std/lang/hooks/unwind_itanium.velt).
fn std_headers(target: &str) -> &'static [&'static str] {
    if target.contains("-windows") {
        &["<errno.h>", "<stdlib.h>", "<string.h>"]
    } else {
        &[
            "<errno.h>",
            "<fcntl.h>",
            "<stdlib.h>",
            "<string.h>",
            "<sys/stat.h>",
            "<unistd.h>",
            "<unwind.h>",
        ]
    }
}

/// Check std's own C declarations (those in `std_files`; std at `root`) against the C library's
/// headers ([`std_headers`]) for `target`, as a package's are: `velt native headers --std`, with
/// zig. Builds take them as checked. Nothing is remembered for a record: std has none.
pub fn check_std(
    p: &hir::Program,
    root: &Path,
    std_files: &HashSet<FileId>,
    target: &str,
    diags: &mut Diagnostics,
) {
    let mut decls = vec![];
    for def in &p.defs {
        match decl_of(p, def) {
            Some(("c", d)) if std_files.contains(&d.span.file) => decls.push(d),
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
        defines: &[],
    };
    check_library(root, "c", read, &decls, (zig_target, target), diags);
}
