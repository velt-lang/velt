//! `std/fs_stream`: files read and written in chunks, for data too large to hold at once.
//!
//! A reader is a buffered `std::fs::File` behind a mutex, limited to a range of the file when
//! it was opened with one (`openRead(path, { start, end })`); reads run on tokio's blocking pool
//! (like the rest of std/fs) and return one chunk; text and lines are decoded and split in std
//! (std/fs/line_buffer.velt, over 64 KiB chunks read here). A writer is a `BufWriter`: `write`
//! appends to its buffer (flushing to the file as it fills), `flush` pushes the buffer out, and
//! `close` flushes and closes, reporting errors. Releasing a writer without `close` still
//! flushes, on the releasing thread, but errors are then lost. Handles are keys into handle
//! tables (`crate::registry`); in-flight operations hold their own `Arc`, and a released handle
//! (through any copy of the Velt struct) fails with `EBADF`.

use super::ops::{data_arg, open, path_arg, stats_of, VeltStat};
use crate::bytes::VeltBytes;
use crate::net::tcp::DEFAULT_READ;
use crate::registry::{Key, Registry};
use crate::result::{fs_error, IoResult};
use crate::str::VeltStr;
use crate::task::leaf::{blocking_leaf, new_leaf};
use crate::task::VeltFut;
use std::fs::{File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Take, Write};
use std::sync::{Mutex, MutexGuard};

/// Buffer size of readers and writers.
const BUFFER: usize = 64 * 1024;

/// The file under a reader: the whole file, or the range it was opened with.
type Source = BufReader<Take<File>>;

/// An open file being read.
pub struct ReaderObj {
    inner: Mutex<Source>,
    /// The file's metadata when it was opened (`fstat` in the same hop as the open).
    opened: VeltStat,
}

/// An open file being written; `None` once closed.
pub struct WriterObj {
    inner: Mutex<Option<BufWriter<File>>>,
}

/// Opaque reader handle.
pub type ReaderHandle = Key<ReaderObj>;
/// Opaque writer handle.
pub type WriterHandle = Key<WriterObj>;

static READERS: Registry<ReaderObj> = Registry::new();
static WRITERS: Registry<WriterObj> = Registry::new();

/// A poisoned lock only means another operation panicked (which ends the process anyway).
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// `openRead(path)` → `IoResult<VeltFileReader*>`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_open_read(path: *const VeltStr) -> *mut VeltFut {
    open_reader(path_arg(path), 0, u64::MAX)
}

/// `openRead(path, { start, end })` → `IoResult<VeltFileReader*>`: reads bytes `start..=end`
/// (inclusive, as Node's `createReadStream`; `end` `u64::MAX` = to the end of the file), then
/// reports the end of the file. `end < start` reads nothing (std checks the arguments). A
/// `start` past the end of the file reads nothing.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_open_read_range(
    path: *const VeltStr,
    start: u64,
    end: u64,
) -> *mut VeltFut {
    open_reader(path_arg(path), start, end)
}

fn open_reader(p: std::path::PathBuf, start: u64, end: u64) -> *mut VeltFut {
    blocking_leaf(move || {
        // No file has a byte past `i64::MAX` (where `lseek` stops): such a start reads nothing.
        let beyond = start > i64::MAX as u64;
        let opened = open(&p, OpenOptions::new().read(true)).and_then(|mut f| {
            let meta = f.metadata()?;
            if start > 0 && !beyond {
                f.seek(SeekFrom::Start(start))?;
            }
            Ok((f, stats_of(meta)))
        });
        let opened = opened.map_err(|e| fs_error(e, "open", &p, None));
        let limit = if beyond {
            0
        } else if end == u64::MAX {
            u64::MAX
        } else {
            (end + 1).saturating_sub(start)
        };
        IoResult::from_io(opened, |(f, meta)| {
            READERS.insert(ReaderObj {
                inner: Mutex::new(BufReader::with_capacity(BUFFER, f.take(limit))),
                opened: meta,
            })
        })
    })
}

/// `openedStats()` → `IoResult<VeltStat>`: the file's metadata when it was opened (taken with
/// the open, so this costs no I/O): the size and modification time of the file this reader
/// reads, even if its path names another one now. The size is the whole file's, whatever range
/// the reader was opened with. `EBADF` once closed.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_reader_opened_stats(
    r: ReaderHandle,
    out: *mut IoResult<VeltStat>,
) {
    match READERS.get(r) {
        Some(obj) => IoResult::ok(obj.opened),
        None => crate::registry::closed(),
    }
    .write_to(out);
}

fn read_chunk(r: &mut Source, max: u64) -> io::Result<Vec<u8>> {
    let size = if max == 0 {
        DEFAULT_READ
    } else {
        max.min(16 << 20)
    };
    let mut buf = vec![0u8; size as usize];
    let n = r.read(&mut buf)?;
    buf.truncate(n);
    Ok(buf)
}

/// `read(max)` → `IoResult<VeltBytes>`: 1..=max bytes, empty at end of file (`max` 0 = 64 KiB).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_reader_read(r: ReaderHandle, max: u64) -> *mut VeltFut {
    READERS.op::<VeltBytes>(r, |obj| {
        blocking_leaf(move || {
            let mut g = lock(&obj.inner);
            IoResult::from_io(read_chunk(&mut g, max), VeltBytes::from_vec)
        })
    })
}

/// Release a reader handle (the file closes once no read is in flight); releasing it again,
/// through any copy, is a no-op.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_reader_close(r: ReaderHandle) {
    READERS.remove(r);
}

/// `openWrite(path, append)` → `IoResult<VeltFileWriter*>`: creates the file, truncating it
/// unless `append`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_open_write(path: *const VeltStr, append: u8) -> *mut VeltFut {
    let p = path_arg(path);
    blocking_leaf(move || {
        let mut o = OpenOptions::new();
        o.create(true);
        if append != 0 {
            o.append(true);
        } else {
            o.write(true).truncate(true);
        }
        let opened = open(&p, &o).map_err(|e| fs_error(e, "open", &p, None));
        IoResult::from_io(opened, |f| {
            WRITERS.insert(WriterObj {
                inner: Mutex::new(Some(BufWriter::with_capacity(BUFFER, f))),
            })
        })
    })
}

fn closed() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "the file writer is closed")
}

/// Runs `op` on the open writer on the blocking pool → `IoResult<()>`.
unsafe fn with_writer(
    w: WriterHandle,
    op: impl FnOnce(&mut Option<BufWriter<File>>) -> io::Result<()> + Send + 'static,
) -> *mut VeltFut {
    WRITERS.op::<()>(w, |obj| {
        blocking_leaf(move || IoResult::from_io(op(&mut lock(&obj.inner)), |()| ()))
    })
}

/// `write(data)` (a string, copied) → `IoResult<()>`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_writer_write(
    w: WriterHandle,
    data: *const VeltStr,
) -> *mut VeltFut {
    write_data(w, data_arg(data))
}

/// # Safety
/// As for [`with_writer`].
unsafe fn write_data(w: WriterHandle, data: Vec<u8>) -> *mut VeltFut {
    with_writer(w, move |f| f.as_mut().ok_or_else(closed)?.write_all(&data))
}

/// `writeBytes(data)`: the same operation bound with a `u8[]` parameter (a Velt module binds
/// each symbol with one signature, so string and bytes writes need two names).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_writer_write_bytes(
    w: WriterHandle,
    data: *const VeltBytes,
) -> *mut VeltFut {
    write_data(w, (*data).as_bytes().to_vec())
}

/// `flush()` → `IoResult<()>`: buffered data reaches the OS.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_writer_flush(w: WriterHandle) -> *mut VeltFut {
    with_writer(w, |f| f.as_mut().ok_or_else(closed)?.flush())
}

/// `close()` → `IoResult<()>`: flush, then close the file; later writes fail with `EPIPE`.
/// Closing twice is fine, also through another copy of an already released handle.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_writer_close(w: WriterHandle) -> *mut VeltFut {
    if WRITERS.get(w).is_none() {
        return new_leaf(async { IoResult::ok(()) });
    }
    with_writer(w, |f| match f.take() {
        Some(mut b) => b.flush(),
        None => Ok(()),
    })
}

/// Release a writer handle; if it was not closed, the last release flushes (errors ignored).
/// Releasing it again, through any copy, is a no-op.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_writer_free(w: WriterHandle) {
    WRITERS.remove(w);
}
