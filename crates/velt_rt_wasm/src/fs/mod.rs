//! `std/fs` (rt_abi_async.md §5) over WASI files: the shared operations (ops.rs), async variants
//! that run the operation on their first poll (there is no blocking
//! pool on one thread), and the `*_sync` functions of whole.rs and tree.rs. In the browser every
//! operation fails with the error `std` reports there. Renaming, linking and copying are std
//! code on WebAssembly too (std/fs/paths/wasi.velt and js.velt).

mod ops;
mod tree;
mod whole;

pub use ops::VeltStat;
pub use tree::{kind, VeltDirents};

use crate::result::{fs_error, IoResult};
use std::ffi::OsString;
use std::io;
use std::path::Path;

fn unit(r: io::Result<()>) -> IoResult<()> {
    IoResult::from_io(r, |()| ())
}

/// Node's error message for a failed `syscall` on `path` (`ENOENT: …, open 'x'`).
fn at<'a>(syscall: &'static str, path: &'a Path) -> impl FnOnce(io::Error) -> io::Error + 'a {
    move |e| fs_error(e, syscall, path, None)
}

/// An entry name as text: no copy when it is UTF-8 (one U+FFFD per invalid sequence otherwise).
fn name_text(name: OsString) -> String {
    name.into_string()
        .unwrap_or_else(|n| n.to_string_lossy().into_owned())
}
