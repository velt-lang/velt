//! `COPY … FROM STDIN` and `COPY … TO STDOUT`, streamed (rt_abi_async.md §14.15).
//!
//! `client.copyFrom(sql)` starts the copy and returns a writer: every `write` hands its chunk to
//! tokio-postgres' sink, which batches small chunks into ~4 KiB messages, and `end()` finishes
//! the copy (the server's row count). A writer released without `end()` aborts the copy (the
//! server rolls the statement back). `client.copyTo(sql)` returns a reader whose `read()` yields
//! the data as it arrives: every chunk already received, up to [`READ_BATCH`] bytes, in one call
//! (in the text and CSV formats a server message is one row), `""` / empty at the end.
//!
//! Writers and readers are Copy structs in Velt, so their handles are registry keys. The
//! connection is busy until the copy ends; a failure counts against the client like any other
//! operation (so an enclosing `transaction` rolls back).

use super::client::{io_result, ClientHandle, ClientObj, CLIENTS};
use super::connection::Conn;
use super::error::PgError;
use crate::bytes::VeltBytes;
use crate::registry::{Key, Registry};
use crate::str::VeltStr;
use crate::task::leaf::new_leaf;
use crate::task::VeltFut;
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_postgres::{CopyInSink, CopyOutStream};

/// A `read()` returns at most about this many bytes (more if one message is larger).
const READ_BATCH: usize = 64 * 1024;

/// A running `COPY … FROM STDIN`.
pub struct CopyInObj {
    client: Arc<ClientObj>,
    sink: Mutex<Pin<Box<CopyInSink<Bytes>>>>,
}

/// A running `COPY … TO STDOUT`.
pub struct CopyOutObj {
    client: Arc<ClientObj>,
    /// `None` once the stream ended: polling it again would read the statement's completion
    /// message, which it reports as unexpected.
    stream: Mutex<Option<Pin<Box<CopyOutStream>>>>,
}

/// Opaque writer handle.
pub type CopyInHandle = Key<CopyInObj>;
/// Opaque reader handle.
pub type CopyOutHandle = Key<CopyOutObj>;

static WRITERS: Registry<CopyInObj> = Registry::new();
static READERS: Registry<CopyOutObj> = Registry::new();

/// A future failing at once with `ECLOSED` (std's `PgError` for a released writer or reader).
fn closed<T: Send + 'static>(what: &'static str) -> *mut VeltFut {
    new_leaf(async move { io_result::<T>(Err(PgError::closed(what))) })
}

/// Counts a failure against the client (for `transaction`), then passes it on.
fn noted<T>(client: &ClientObj, r: Result<T, PgError>) -> Result<T, PgError> {
    if let Err(e) = &r {
        client.note(e.clone());
    }
    r
}

/// Starts a copy on the client's connection: the client and the started copy.
async fn start<T, F, Fut>(
    client: Option<Arc<ClientObj>>,
    begin: F,
) -> Result<(Arc<ClientObj>, T), PgError>
where
    F: FnOnce(Arc<Conn>) -> Fut,
    Fut: std::future::Future<Output = Result<T, PgError>>,
{
    let client = client.ok_or_else(|| PgError::closed("client"))?;
    let r = match client.conn() {
        Ok(conn) => begin(conn).await,
        Err(e) => Err(e),
    };
    noted(&client, r).map(|copy| (client, copy))
}

/// `client.copyFrom(sql)` → `IoResult<VeltPgCopyIn>`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_pg_copy_from(
    client: ClientHandle,
    sql: *const VeltStr,
) -> *mut VeltFut {
    let (client, sql) = (CLIENTS.get(client), super::client::text(sql));
    new_leaf(async move {
        let r = start(client, |conn| async move { conn.copy_in(&sql).await }).await;
        io_result(r.map(|(client, sink)| {
            WRITERS.insert(CopyInObj {
                client,
                sink: Mutex::new(Box::pin(sink)),
            })
        }))
    })
}

/// Sends one chunk (copied) → `IoResult<()>`.
unsafe fn write(w: CopyInHandle, data: Bytes) -> *mut VeltFut {
    let Some(obj) = WRITERS.get(w) else {
        return closed::<()>("copy writer");
    };
    new_leaf(async move {
        let r = obj
            .sink
            .lock()
            .await
            .feed(data)
            .await
            .map_err(PgError::from);
        io_result(noted(&obj.client, r))
    })
}

/// `writer.write(text)` → `IoResult<()>`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_pg_copy_write(
    w: CopyInHandle,
    data: *const VeltStr,
) -> *mut VeltFut {
    write(w, Bytes::copy_from_slice((*data).text_lossy().as_bytes()))
}

/// `writer.writeBytes(bytes)`: the same operation bound with a `u8[]` parameter.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_pg_copy_write_bytes(
    w: CopyInHandle,
    data: *const VeltBytes,
) -> *mut VeltFut {
    velt_rt_pg_copy_write(w, data as *const VeltStr)
}

/// `writer.end()` → `IoResult<i64>`: finishes the copy (releasing the handle); the rows copied.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_pg_copy_end(w: CopyInHandle) -> *mut VeltFut {
    let Some(obj) = WRITERS.remove(w) else {
        return closed::<i64>("copy writer");
    };
    new_leaf(async move {
        let r = obj.sink.lock().await.as_mut().finish().await;
        io_result(noted(&obj.client, r.map_err(PgError::from)).map(|n| n as i64))
    })
}

/// `writer.abort()`: releases the handle without finishing; the server aborts the copy.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_pg_copy_abort(w: CopyInHandle) {
    WRITERS.remove(w);
}

/// `client.copyTo(sql)` → `IoResult<VeltPgCopyOut>`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_pg_copy_to(
    client: ClientHandle,
    sql: *const VeltStr,
) -> *mut VeltFut {
    let (client, sql) = (CLIENTS.get(client), super::client::text(sql));
    new_leaf(async move {
        let r = start(client, |conn| async move { conn.copy_out(&sql).await }).await;
        io_result(r.map(|(client, stream)| {
            READERS.insert(CopyOutObj {
                client,
                stream: Mutex::new(Some(Box::pin(stream))),
            })
        }))
    })
}

/// The next chunks: whatever has arrived (waiting for the first), up to [`READ_BATCH`] bytes;
/// empty at the end.
async fn read_batch(obj: &CopyOutObj) -> Result<Vec<u8>, PgError> {
    let mut slot = obj.stream.lock().await;
    let mut out = Vec::new();
    let Some(stream) = slot.as_mut() else {
        return Ok(out);
    };
    let Some(first) = stream.next().await else {
        *slot = None;
        return Ok(out);
    };
    out.extend_from_slice(&first?);
    while out.len() < READ_BATCH {
        match futures_util::FutureExt::now_or_never(stream.next()) {
            Some(Some(chunk)) => out.extend_from_slice(&chunk?),
            Some(None) => {
                *slot = None;
                break;
            }
            None => break,
        }
    }
    Ok(out)
}

/// `reader.read()` → `IoResult<VeltStr>`: the next chunks as text (invalid UTF-8 → U+FFFD),
/// `""` at the end.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_pg_copy_read(r: CopyOutHandle) -> *mut VeltFut {
    let Some(obj) = READERS.get(r) else {
        return closed::<VeltStr>("copy reader");
    };
    new_leaf(async move {
        let data = noted(&obj.client, read_batch(&obj).await);
        io_result(data.map(|d| crate::stdin::decode(&d)))
    })
}

/// `reader.readBytes()` → `IoResult<VeltBytes>`: the next chunks, empty at the end.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_pg_copy_read_bytes(r: CopyOutHandle) -> *mut VeltFut {
    let Some(obj) = READERS.get(r) else {
        return closed::<VeltBytes>("copy reader");
    };
    new_leaf(async move {
        let data = noted(&obj.client, read_batch(&obj).await);
        io_result(data.map(VeltBytes::from_vec))
    })
}

/// `reader.close()`: releases the handle (data not read yet is discarded).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_pg_copy_close(r: CopyOutHandle) {
    READERS.remove(r);
}
