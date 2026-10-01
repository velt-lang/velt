//! A child's stdin/stdout/stderr pipes as `VeltFut`s: chunked reads (bytes, or UTF-8 text
//! completed across chunk boundaries like TCP `readString`), whole writes, closing stdin.

use super::{ChildHandle, ChildObj, CHILDREN};
use crate::bytes::VeltBytes;
use crate::net::tcp::DEFAULT_READ;
use crate::net::utf8::Utf8Decoder;
use crate::result::IoResult;
use crate::str::VeltStr;
use crate::task::leaf::new_leaf;
use crate::task::VeltFut;
use std::io;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

/// An output pipe plus the undecoded tail of a character split across text reads.
pub struct Reader<R> {
    stream: R,
    utf8: Utf8Decoder,
}

impl<R: AsyncRead + Unpin> Reader<R> {
    /// Wraps a pipe.
    pub fn new(stream: R) -> Reader<R> {
        Reader {
            stream,
            utf8: Utf8Decoder::default(),
        }
    }

    async fn read_chunk(&mut self, max: u64) -> io::Result<Vec<u8>> {
        let size = if max == 0 { DEFAULT_READ } else { max };
        let mut buf = vec![0u8; size.min(16 << 20) as usize];
        let n = self.stream.read(&mut buf).await?;
        buf.truncate(n);
        Ok(buf)
    }

    async fn read_text(&mut self, max: u64) -> io::Result<String> {
        loop {
            let chunk = self.read_chunk(max).await?;
            let text = self.utf8.decode(&chunk, chunk.is_empty());
            if !text.is_empty() || chunk.is_empty() {
                return Ok(text);
            }
        }
    }
}

fn not_piped(name: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("the child's {name} is not a pipe (spawn it with {name}: \"pipe\")"),
    )
}

/// Reads from stdout (`which == 1`) or stderr (`2`).
async fn read(obj: &ChildObj, which: u32, max: u64, text: bool) -> io::Result<Vec<u8>> {
    macro_rules! read_from {
        ($pipe:expr, $name:expr) => {{
            let mut guard = $pipe.lock().await;
            let reader = guard.as_mut().ok_or_else(|| not_piped($name))?;
            if text {
                reader.read_text(max).await.map(String::into_bytes)
            } else {
                reader.read_chunk(max).await
            }
        }};
    }
    if which == 2 {
        read_from!(obj.stderr, "stderr")
    } else {
        read_from!(obj.stdout, "stdout")
    }
}

/// `readStdout()`/`readStderr()` bytes → `IoResult<VeltBytes>`: 1..=max bytes, empty = end
/// of stream; `max == 0` ⇒ 64 KiB. `which`: 1 = stdout, 2 = stderr.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_child_read(c: ChildHandle, which: u32, max: u64) -> *mut VeltFut {
    CHILDREN.op::<VeltBytes>(c, |obj| {
        new_leaf(async move {
            IoResult::from_io(read(&obj, which, max, false).await, VeltBytes::from_vec)
        })
    })
}

/// Like `velt_rt_child_read`, decoded as UTF-8 (`IoResult<VeltStr>`; `""` = end of stream).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_child_read_string(
    c: ChildHandle,
    which: u32,
    max: u64,
) -> *mut VeltFut {
    CHILDREN.op::<VeltStr>(c, |obj| {
        new_leaf(
            async move { IoResult::from_io(read(&obj, which, max, true).await, VeltStr::from_vec) },
        )
    })
}

/// `write(data)` to stdin (string or bytes, copied) → `IoResult<()>` once written.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_child_write(
    c: ChildHandle,
    data: *const VeltBytes,
) -> *mut VeltFut {
    let data = (*data).as_bytes().to_vec();
    CHILDREN.op::<()>(c, |obj| {
        new_leaf(async move {
            let mut guard = obj.stdin.lock().await;
            let r = match guard.as_mut() {
                Some(stdin) => stdin.write_all(&data).await,
                None => Err(not_piped("stdin")),
            };
            IoResult::from_io(r, |()| ())
        })
    })
}

/// Closes stdin (the child reads end of stream) → `IoResult<()>`; closing twice is fine.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_child_close_stdin(c: ChildHandle) -> *mut VeltFut {
    CHILDREN.op::<()>(c, |obj| {
        new_leaf(async move {
            let stdin = obj.stdin.lock().await.take();
            match stdin {
                Some(mut s) => IoResult::from_io(s.shutdown().await, |()| ()),
                None => IoResult::ok(()),
            }
        })
    })
}
