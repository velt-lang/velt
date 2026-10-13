//! A connector that stamps each new connection with when it was opened, so `fetch` can tell a
//! reused pooled connection (opened before the request was sent) from a fresh one when a send
//! fails ([`opened_before`]).

use hyper::Uri;
use hyper_util::client::legacy::connect::{Connected, Connection};
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Instant;

/// When a connection was opened (carried in its [`Connected`] extras).
#[derive(Clone, Copy)]
struct Opened(Instant);

/// Wraps connector `C`: its connections report when they were opened.
#[derive(Clone)]
pub(super) struct Stamped<C>(pub C);

impl<C> tower_service::Service<Uri> for Stamped<C>
where
    C: tower_service::Service<Uri>,
    C::Future: Send + 'static,
{
    type Response = StampedIo<C::Response>;
    type Error = C::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, C::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), C::Error>> {
        self.0.poll_ready(cx)
    }

    fn call(&mut self, uri: Uri) -> Self::Future {
        let connecting = self.0.call(uri);
        Box::pin(async move {
            let io = connecting.await?;
            Ok(StampedIo {
                io,
                opened: Instant::now(),
            })
        })
    }
}

/// A connection of [`Stamped`].
pub(super) struct StampedIo<T> {
    io: T,
    opened: Instant,
}

impl<T: Connection> Connection for StampedIo<T> {
    fn connected(&self) -> Connected {
        self.io.connected().extra(Opened(self.opened))
    }
}

impl<T: hyper::rt::Read + Unpin> hyper::rt::Read for StampedIo<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: hyper::rt::ReadBufCursor<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.io).poll_read(cx, buf)
    }
}

impl<T: hyper::rt::Write + Unpin> hyper::rt::Write for StampedIo<T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.io).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.io).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.io).poll_shutdown(cx)
    }

    fn is_write_vectored(&self) -> bool {
        self.io.is_write_vectored()
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.io).poll_write_vectored(cx, bufs)
    }
}

/// Whether the connection a failed send used was opened before `sent` (a reused pooled
/// connection); false when the error carries no connection or it is not stamped.
pub(super) fn opened_before(e: &hyper_util::client::legacy::Error, sent: Instant) -> bool {
    let Some(info) = e.connect_info() else {
        return false;
    };
    let mut extras = hyper::http::Extensions::new();
    info.get_extras(&mut extras);
    extras.get::<Opened>().is_some_and(|o| o.0 < sent)
}
