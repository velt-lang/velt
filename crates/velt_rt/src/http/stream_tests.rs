//! Streamed response bodies: the writer and body on their own, through the C ABI, and on the
//! wire (hyper over a real socket, read with a raw HTTP/1.1 client).

use super::body::RespBody;
use super::response::{velt_rt_http_resp_drop, velt_rt_http_resp_new, RespObj};
use super::stream::*;
use crate::str::VeltStr;
use crate::task::runtime::handle;
use bytes::Bytes;
use futures_util::FutureExt;
use http_body_util::BodyExt;
use hyper::body::Body;
use hyper::header::{CONTENT_LENGTH, CONTENT_TYPE};
use hyper::Response;
use std::future::Future;
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::Arc;

fn block_on<F: Future>(f: F) -> F::Output {
    handle().block_on(f)
}

/// The next frame: `Some(Ok(data))`, `Some(Err(()))` for an aborted body, `None` at the end.
async fn next(body: &mut RespBody) -> Option<Result<Bytes, ()>> {
    let frame = body.frame().await?;
    Some(
        frame
            .map(|f| f.into_data().expect("a data frame"))
            .map_err(|_| ()),
    )
}

fn stream() -> (WriterObj, RespBody) {
    let (writer, body) = WriterObj::new();
    (writer, RespBody::Stream(body))
}

#[test]
fn full_bodies_keep_their_exact_length_and_streams_have_none() {
    let full = RespBody::full(Bytes::from_static(b"hello"));
    assert_eq!(full.size_hint().exact(), Some(5));
    let (_writer, streamed) = stream();
    assert_eq!(streamed.size_hint().exact(), None);
    assert!(!streamed.is_end_stream());
}

#[test]
fn writes_are_buffered_until_flush_and_close_ends_the_body() {
    let (writer, mut body) = stream();
    block_on(async {
        assert!(writer.write(b"<a>"));
        assert!(writer.write(b"<b>"));
        assert!(
            next(&mut body).now_or_never().is_none(),
            "nothing before flush"
        );
        assert!(writer.flush().await);
        assert_eq!(
            next(&mut body).await,
            Some(Ok(Bytes::from_static(b"<a><b>")))
        );
        assert!(writer.write(b"<c>"));
        assert!(writer.close().await);
        assert_eq!(next(&mut body).await, Some(Ok(Bytes::from_static(b"<c>"))));
        assert_eq!(next(&mut body).await, None);
        assert!(!writer.write(b"late"), "closed");
    });
}

#[test]
fn abort_fails_the_body() {
    let (writer, mut body) = stream();
    block_on(async {
        writer.write(b"part");
        assert!(writer.flush().await);
        writer.abort();
        assert_eq!(next(&mut body).await, Some(Ok(Bytes::from_static(b"part"))));
        assert_eq!(next(&mut body).await, Some(Err(())));
    });
}

#[test]
fn a_gone_client_turns_writes_into_no_ops() {
    let (writer, body) = stream();
    drop(body);
    block_on(async {
        assert!(!writer.write(b"x"));
        assert!(!writer.flush().await);
        assert!(!writer.close().await);
    });
    let (writer, body) = stream();
    assert!(writer.write(b"x"));
    drop(body);
    assert!(
        !block_on(writer.flush()),
        "the buffered text is undeliverable"
    );
}

#[test]
fn flush_waits_while_the_client_is_behind() {
    let (writer, mut body) = stream();
    block_on(async {
        let mut flushed = 0;
        loop {
            writer.write(b"chunk");
            if writer.flush().now_or_never().is_none() {
                break;
            }
            flushed += 1;
        }
        assert!(flushed >= 1, "the channel takes some chunks");
        // The flush that waited (and was dropped) kept its text; draining makes room again.
        for _ in 0..flushed {
            assert_eq!(
                next(&mut body).await,
                Some(Ok(Bytes::from_static(b"chunk")))
            );
        }
        assert!(writer.flush().await);
        assert_eq!(
            next(&mut body).await,
            Some(Ok(Bytes::from_static(b"chunk")))
        );
    });
}

#[test]
fn a_large_buffer_is_sent_without_flush() {
    let (writer, mut body) = stream();
    let big = vec![b'x'; 20_000];
    assert!(writer.write(&big));
    let chunk = block_on(next(&mut body)).unwrap().unwrap();
    assert_eq!(chunk.len(), big.len());
}

#[test]
fn the_abi_opens_writes_and_releases_writers() {
    unsafe {
        let r = velt_rt_http_resp_new(201);
        let w = velt_rt_http_resp_stream_open(r);
        let content_type = super::response::with(r, |r| r.headers()[CONTENT_TYPE].clone());
        assert_eq!(content_type.unwrap(), "text/plain; charset=utf-8");
        let text = VeltStr::from_bytes(b"hi");
        assert_eq!(velt_rt_http_resp_stream_write(w, &text), 1);
        velt_rt_http_resp_stream_abort(w);
        assert_eq!(velt_rt_http_resp_stream_write(w, &text), 0, "released");
        velt_rt_http_resp_stream_abort(w);
        velt_rt_http_resp_drop(r);
    }
}

/// Serves one connection with `resp` from a hyper HTTP/1.1 server; returns the port.
fn serve_once(resp: RespObj) -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let resp = Arc::new(parking_lot::Mutex::new(Some(resp)));
    handle().spawn(async move {
        let listener = tokio::net::TcpListener::from_std(listener).unwrap();
        let (io, _) = listener.accept().await.unwrap();
        let svc = hyper::service::service_fn(move |_req| {
            let resp = resp.lock().take().expect("one request");
            async move { Ok::<_, std::convert::Infallible>(resp) }
        });
        let io = hyper_util::rt::TokioIo::new(io);
        let _ = hyper::server::conn::http1::Builder::new()
            .serve_connection(io, svc)
            .await;
    });
    port
}

#[test]
fn headers_go_out_before_the_first_chunk_and_the_body_is_chunked() {
    let (writer, body) = stream();
    let mut resp = Response::new(body);
    resp.headers_mut()
        .insert(CONTENT_TYPE, "text/html".parse().unwrap());
    let port = serve_once(resp);
    let s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    let mut r = BufReader::new(s.try_clone().unwrap());
    (&s).write_all(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n")
        .unwrap();
    // Nothing was written yet: the head alone arrives.
    let mut head = String::new();
    loop {
        let mut line = String::new();
        r.read_line(&mut line).unwrap();
        head.push_str(&line.to_ascii_lowercase());
        if line == "\r\n" {
            break;
        }
    }
    assert!(head.starts_with("http/1.1 200"), "{head}");
    assert!(head.contains("transfer-encoding: chunked"), "{head}");
    assert!(head.contains("content-type: text/html"), "{head}");
    assert!(!head.contains(CONTENT_LENGTH.as_str()), "{head}");
    block_on(async {
        writer.write(b"one");
        assert!(writer.flush().await);
    });
    let mut line = String::new();
    r.read_line(&mut line).unwrap();
    assert_eq!(line, "3\r\n");
    block_on(async {
        writer.write(b"two!");
        assert!(writer.close().await);
    });
    let expected = "one\r\n4\r\ntwo!\r\n0\r\n\r\n";
    let mut rest = vec![0; expected.len()];
    r.read_exact(&mut rest).unwrap();
    assert_eq!(String::from_utf8(rest).unwrap(), expected);
}
