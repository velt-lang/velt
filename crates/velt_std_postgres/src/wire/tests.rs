//! The wire against a scripted server (an in-memory duplex stream): injected groups go between
//! driver requests and get exactly their own replies.

use super::{Wire, WireStream};
use bytes::Bytes;
use std::future::Future;
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

fn msg(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut m = vec![tag];
    m.extend_from_slice(&(body.len() as u32 + 4).to_be_bytes());
    m.extend_from_slice(body);
    m
}

fn untagged(code: u32) -> Vec<u8> {
    let mut m = 8u32.to_be_bytes().to_vec();
    m.extend_from_slice(&code.to_be_bytes());
    m
}

fn ready() -> Vec<u8> {
    msg(b'Z', b"I")
}

fn cat(parts: &[&[u8]]) -> Vec<u8> {
    parts.concat()
}

fn run(test: impl Future<Output = ()>) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime")
        .block_on(test);
}

async fn expect(server: &mut DuplexStream, want: &[u8]) {
    let mut got = vec![0; want.len()];
    server.read_exact(&mut got).await.unwrap();
    assert_eq!(got, want);
}

async fn expect_driver(driver: &mut WireStream<DuplexStream>, want: &[u8]) {
    let mut got = vec![0; want.len()];
    driver.read_exact(&mut got).await.unwrap();
    assert_eq!(got, want);
}

/// A started connection: the driver end, the server end and the wire handle.
async fn started() -> (WireStream<DuplexStream>, DuplexStream, Wire) {
    let (a, mut server) = tokio::io::duplex(1 << 16);
    let wire = Wire::new();
    let mut driver = WireStream::new(a, wire.shared());
    let startup = cat(&[&untagged(196608), b""]);
    driver.write_all(&startup).await.unwrap();
    expect(&mut server, &startup).await;
    let reply = cat(&[&msg(b'R', &[0, 0, 0, 0]), &msg(b'S', b"a\0b\0"), &ready()]);
    server.write_all(&reply).await.unwrap();
    expect_driver(&mut driver, &reply).await;
    (driver, server, wire)
}

#[test]
fn groups_go_between_requests_and_get_their_replies() {
    run(async {
        let (mut driver, mut server, wire) = started().await;
        let request = cat(&[&msg(b'B', b"x"), &msg(b'E', b"y"), &msg(b'S', b"")]);
        let group = cat(&[&msg(b'B', b"g"), &msg(b'E', b"h"), &msg(b'S', b"")]);
        // Half a request is out when the group is queued: it waits for the request's Sync.
        driver.write_all(&request[..7]).await.unwrap();
        let replies = wire.send(Bytes::from(group.clone())).unwrap();
        let next = msg(b'Q', b"SELECT 1\0");
        driver
            .write_all(&cat(&[&request[7..], &next]))
            .await
            .unwrap();
        driver.flush().await.unwrap();
        expect(&mut server, &cat(&[&request, &group, &next])).await;

        let first = cat(&[&msg(b'2', b""), &msg(b'D', b"\0\0"), &ready()]);
        let mine = cat(&[&msg(b'2', b""), &msg(b'C', b"SELECT 1\0"), &ready()]);
        let notice = msg(b'N', b"Mhi\0\0");
        let last = cat(&[&msg(b'C', b"SELECT 1\0"), &ready()]);
        // A notice in the middle of the group's replies is the driver's.
        server
            .write_all(&cat(&[&first, &mine[..5], &notice, &mine[5..], &last]))
            .await
            .unwrap();
        expect_driver(&mut driver, &cat(&[&first, &notice, &last])).await;
        assert_eq!(&replies.await.unwrap()[..], &mine[..]);
    });
}

#[test]
fn an_idle_driver_still_sends_groups() {
    run(async {
        let (mut driver, mut server, wire) = started().await;
        let group = cat(&[&msg(b'B', b"g"), &msg(b'S', b"")]);
        let replies = wire.send(Bytes::from(group.clone())).unwrap();
        // The connection task polls the stream for reads and flushes.
        driver.flush().await.unwrap();
        expect(&mut server, &group).await;
        let mine = cat(&[&msg(b'2', b""), &ready()]);
        let theirs = cat(&[&msg(b'S', b"k\0v\0")]);
        server.write_all(&cat(&[&mine, &theirs])).await.unwrap();
        expect_driver(&mut driver, &theirs).await;
        assert_eq!(&replies.await.unwrap()[..], &mine[..]);
    });
}

#[test]
fn no_group_goes_into_a_copy_in() {
    run(async {
        let (mut driver, mut server, wire) = started().await;
        let start = cat(&[&msg(b'B', b"c"), &msg(b'E', b"e"), &msg(b'S', b"")]);
        driver.write_all(&start).await.unwrap();
        expect(&mut server, &start).await;
        let copying = cat(&[&msg(b'2', b""), &msg(b'G', b"\0\0\0")]);
        server.write_all(&copying).await.unwrap();
        expect_driver(&mut driver, &copying).await;

        let group = cat(&[&msg(b'B', b"g"), &msg(b'S', b"")]);
        let replies = wire.send(Bytes::from(group.clone())).unwrap();
        let data = cat(&[&msg(b'd', b"1\n"), &msg(b'c', b""), &msg(b'S', b"")]);
        driver.write_all(&data).await.unwrap();
        driver.flush().await.unwrap();
        expect(&mut server, &cat(&[&data, &group])).await;

        // One ReadyForQuery for the copy (the Sync during it was ignored), then the group's.
        let done = cat(&[&msg(b'C', b"COPY 1\0"), &ready()]);
        let mine = cat(&[&msg(b'2', b""), &ready()]);
        server.write_all(&cat(&[&done, &mine])).await.unwrap();
        expect_driver(&mut driver, &done).await;
        assert_eq!(&replies.await.unwrap()[..], &mine[..]);
    });
}

#[test]
fn the_tls_answer_is_one_byte() {
    run(async {
        let (a, mut server) = tokio::io::duplex(1024);
        let wire = Wire::new();
        let mut driver = WireStream::new(a, wire.shared());
        driver.write_all(&untagged(80877103)).await.unwrap();
        expect(&mut server, &untagged(80877103)).await;
        let reply = cat(&[b"N", &msg(b'R', &[0, 0, 0, 0])]);
        server.write_all(&reply).await.unwrap();
        expect_driver(&mut driver, &reply).await;
    });
}

#[test]
fn a_closed_connection_fails_its_groups() {
    run(async {
        let (mut driver, mut server, wire) = started().await;
        let group = cat(&[&msg(b'B', b"g"), &msg(b'S', b"")]);
        let sent = wire.send(Bytes::from(group.clone())).unwrap();
        driver.flush().await.unwrap();
        expect(&mut server, &group).await;
        drop(server);
        let mut rest = Vec::new();
        driver.read_to_end(&mut rest).await.unwrap();
        assert!(rest.is_empty());
        assert!(sent.await.is_err());
        assert!(wire.send(Bytes::from(group)).is_err());
    });
}
