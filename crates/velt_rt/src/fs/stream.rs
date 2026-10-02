//! `std/fs_stream`: files read and written in chunks, for data too large to hold at once.
//!
//! A reader is a buffered `std::fs::File` behind a mutex; reads run on tokio's blocking pool
//! (like the rest of std/fs) and return one chunk, one UTF-8 text chunk (characters split
//! across chunks are completed on the next read) or one line. Like stdin's read-ahead, a line
//! already in the reader's buffer is returned without the hop to the blocking pool. A writer is a `BufWriter`: `write`
//! appends to its buffer (flushing to the file as it fills), `flush` pushes the buffer out, and
//! `close` flushes and closes, reporting errors. Releasing a writer without `close` still
//! flushes, on the releasing thread, but errors are then lost. Handles are keys into handle
//! tables (`crate::registry`); in-flight operations hold their own `Arc`, and a released handle
//! (through any copy of the Velt struct) fails with `EBADF`.

use super::ops::{data_arg, path_arg};
use crate::bytes::VeltBytes;
use crate::net::tcp::DEFAULT_READ;
use crate::net::utf8::Utf8Decoder;
use crate::registry::{Key, Registry};
use crate::result::{fs_error, IoResult};
use crate::stdin::LineRead;
use crate::str::VeltStr;
use crate::task::leaf::{blocking_leaf, new_leaf, run_blocking};
use crate::task::VeltFut;
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, BufWriter, Read, Write};
use std::sync::{Mutex, MutexGuard};

/// Buffer size of readers and writers.
const BUFFER: usize = 64 * 1024;

/// An open file being read.
pub struct ReaderObj {
    inner: Mutex<(BufReader<File>, Utf8Decoder)>,
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
    let p = path_arg(path);
    blocking_leaf(move || {
        let opened = File::open(&p).map_err(|e| fs_error(e, "open", &p, None));
        IoResult::from_io(opened, |f| {
            READERS.insert(ReaderObj {
                inner: Mutex::new((BufReader::with_capacity(BUFFER, f), Utf8Decoder::default())),
            })
        })
    })
}

fn read_chunk(r: &mut BufReader<File>, max: u64) -> io::Result<Vec<u8>> {
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
            IoResult::from_io(read_chunk(&mut g.0, max), VeltBytes::from_vec)
        })
    })
}

/// `readString(max)` → `IoResult<VeltStr>`: the next chunk as UTF-8, `""` at end of file.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_reader_read_string(r: ReaderHandle, max: u64) -> *mut VeltFut {
    let Some(obj) = READERS.get(r) else {
        return crate::registry::closed_leaf::<VeltStr>();
    };
    blocking_leaf(move || {
        let mut g = lock(&obj.inner);
        let text = loop {
            let (reader, dec) = &mut *g;
            match read_chunk(reader, max) {
                Err(e) => break Err(e),
                Ok(chunk) => {
                    let t = dec.decode(&chunk, chunk.is_empty());
                    if !t.is_empty() || chunk.is_empty() {
                        break Ok(t);
                    }
                }
            }
        };
        IoResult::from_io(text, |t| VeltStr::from_vec(t.into_bytes()))
    })
}

/// `readLine()` → `LineRead` (§14.4): the next line without `\n`/`\r\n`; `eof` at end of file.
/// Do not mix with `readString` on one reader (text chunks may hold back a split character).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_reader_read_line(r: ReaderHandle) -> *mut VeltFut {
    let Some(obj) = READERS.get(r) else {
        return new_leaf(async {
            LineRead {
                line: crate::registry::closed(),
                eof: 0,
            }
        });
    };
    new_leaf(async move {
        match buffered_line(&obj) {
            Some(line) => line,
            None => run_blocking(move || read_line(&mut lock(&obj.inner).0)).await,
        }
    })
}

/// The next line if the reader's buffer already holds all of it (no I/O). `try_lock`: if
/// another read holds the reader, wait for it on the blocking pool rather than on this worker.
fn buffered_line(obj: &ReaderObj) -> Option<LineRead> {
    let mut g = obj.inner.try_lock().ok()?;
    let buf = g.0.buffer();
    let end = memchr::memchr(b'\n', buf)?;
    let line = &buf[..end];
    let line = crate::stdin::decode(line.strip_suffix(b"\r").unwrap_or(line));
    g.0.consume(end + 1);
    Some(LineRead {
        line: IoResult::ok(line),
        eof: 0,
    })
}

/// Reads the next line, blocking.
fn read_line(r: &mut BufReader<File>) -> LineRead {
    let mut buf = Vec::new();
    let res = r.read_until(b'\n', &mut buf);
    let eof = matches!(res, Ok(0)) as u8;
    if buf.ends_with(b"\n") {
        buf.pop();
        if buf.ends_with(b"\r") {
            buf.pop();
        }
    }
    LineRead {
        line: IoResult::from_io(res, |_| crate::stdin::decode(&buf)),
        eof,
    }
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
        let opened = o.open(&p).map_err(|e| fs_error(e, "open", &p, None));
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

/// `write(data)` (string or bytes, copied) → `IoResult<()>`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_writer_write(
    w: WriterHandle,
    data: *const VeltStr,
) -> *mut VeltFut {
    let data = data_arg(data);
    with_writer(w, move |f| f.as_mut().ok_or_else(closed)?.write_all(&data))
}

/// `writeBytes(data)`: the same operation bound with a `u8[]` parameter (a Velt module binds
/// each symbol with one signature, so string and bytes writes need two names).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fs_writer_write_bytes(
    w: WriterHandle,
    data: *const VeltBytes,
) -> *mut VeltFut {
    velt_rt_fs_writer_write(w, data as *const VeltStr)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Every line of `data` read through a reader with a tiny buffer, mixing lines taken from
    /// the buffer with blocking reads.
    fn lines(data: &[u8]) -> Vec<String> {
        let path = std::env::temp_dir().join(format!("velt-readline-{}", std::process::id()));
        std::fs::write(&path, data).unwrap();
        let file = File::open(&path).unwrap();
        let obj = ReaderObj {
            inner: Mutex::new((BufReader::with_capacity(8, file), Utf8Decoder::default())),
        };
        let mut out = vec![];
        loop {
            let r = buffered_line(&obj).unwrap_or_else(|| read_line(&mut lock(&obj.inner).0));
            if r.eof == 1 {
                break;
            }
            assert_eq!(r.line.err.code, 0);
            // SAFETY: `code == 0`: the value is a live string built by the reader (leaked, as
            // test strings are).
            let bytes = unsafe { r.line.value.assume_init_ref().as_bytes() };
            out.push(String::from_utf8_lossy(bytes).into_owned());
        }
        std::fs::remove_file(&path).unwrap();
        out
    }

    #[test]
    fn lines_from_the_buffer_and_from_the_file_agree() {
        let long = "x".repeat(40);
        let data = format!("ab\r\ncd\n\n{long}\nü\u{2028}é\r\nlast");
        assert_eq!(
            lines(data.as_bytes()),
            ["ab", "cd", "", long.as_str(), "ü\u{2028}é", "last"]
        );
        assert_eq!(lines(b"one\n"), ["one"]);
        assert_eq!(lines(b"bad \xff\n"), ["bad \u{fffd}"]);
    }
}
