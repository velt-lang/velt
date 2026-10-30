//! `COPY … FROM STDIN` and `COPY … TO STDOUT`, streamed (docs/native_abi.md "COPY").
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

use super::client::{io_result, ClientObj, CLIENTS};
use super::connection::Conn;
use super::error::PgError;
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_postgres::{CopyInSink, CopyOutStream};
use velt_native::handles::{Key, Registry};
use velt_native::{export, Error, Str};

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
#[export(async)]
async fn velt_postgres__copy_from(client: u64, sql: Str) -> Result<u64, Error> {
    let client = CLIENTS.get(Key::from_bits(client));
    let r = start(
        client,
        |conn| async move { conn.copy_in(&sql.to_str()).await },
    )
    .await;
    io_result(r.map(|(client, sink)| {
        WRITERS
            .insert(CopyInObj {
                client,
                sink: Mutex::new(Box::pin(sink)),
            })
            .bits()
    }))
}

/// Sends one chunk → `IoStatus`.
async fn write(w: u64, data: Bytes) -> Result<(), Error> {
    let Some(obj) = WRITERS.get(Key::from_bits(w)) else {
        return io_result(Err(PgError::closed("copy writer")));
    };
    let r = obj
        .sink
        .lock()
        .await
        .feed(data)
        .await
        .map_err(PgError::from);
    io_result(noted(&obj.client, r))
}

/// `writer.write(text)` → `IoStatus` (the string itself is sent, no copy).
#[export(async)]
async fn velt_postgres__copy_write(w: u64, data: Str) -> Result<(), Error> {
    write(w, Bytes::from_owner(data)).await
}

/// `writer.writeBytes(bytes)`: the same with a `u8[]`.
#[export(async)]
async fn velt_postgres__copy_write_bytes(w: u64, data: Vec<u8>) -> Result<(), Error> {
    write(w, Bytes::from(data)).await
}

/// `writer.end()` → `IoResult<i64>`: finishes the copy (releasing the handle); the rows copied.
#[export(async)]
async fn velt_postgres__copy_end(w: u64) -> Result<i64, Error> {
    let Some(obj) = WRITERS.remove(Key::from_bits(w)) else {
        return io_result(Err(PgError::closed("copy writer")));
    };
    let r = obj.sink.lock().await.as_mut().finish().await;
    io_result(noted(&obj.client, r.map_err(PgError::from)).map(|n| n as i64))
}

/// `writer.abort()`: releases the handle without finishing; the server aborts the copy.
#[export]
fn velt_postgres__copy_abort(w: u64) {
    WRITERS.remove(Key::from_bits(w));
}

/// `client.copyTo(sql)` → `IoResult<VeltPgCopyOut>`.
#[export(async)]
async fn velt_postgres__copy_to(client: u64, sql: Str) -> Result<u64, Error> {
    let client = CLIENTS.get(Key::from_bits(client));
    let r = start(
        client,
        |conn| async move { conn.copy_out(&sql.to_str()).await },
    )
    .await;
    io_result(r.map(|(client, stream)| {
        READERS
            .insert(CopyOutObj {
                client,
                stream: Mutex::new(Some(Box::pin(stream))),
            })
            .bits()
    }))
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

/// `reader.read()` → `IoResult<string>`: the next chunks as text (invalid UTF-8 → U+FFFD),
/// `""` at the end.
#[export(async)]
async fn velt_postgres__copy_read(r: u64) -> Result<String, Error> {
    let Some(obj) = READERS.get(Key::from_bits(r)) else {
        return io_result(Err(PgError::closed("copy reader")));
    };
    let data = noted(&obj.client, read_batch(&obj).await);
    io_result(data.map(|d| match String::from_utf8(d) {
        Ok(s) => s,
        Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
    }))
}

/// `reader.readBytes()` → `IoResult<u8[]>`: the next chunks, empty at the end.
#[export(async)]
async fn velt_postgres__copy_read_bytes(r: u64) -> Result<Vec<u8>, Error> {
    let Some(obj) = READERS.get(Key::from_bits(r)) else {
        return io_result(Err(PgError::closed("copy reader")));
    };
    io_result(noted(&obj.client, read_batch(&obj).await))
}

/// `reader.close()`: releases the handle (data not read yet is discarded).
#[export]
fn velt_postgres__copy_close(r: u64) {
    READERS.remove(Key::from_bits(r));
}
