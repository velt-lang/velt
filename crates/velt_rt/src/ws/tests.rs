//! Tests of the WebSocket codec over loopback sockets (`mod.rs`, `writer.rs`).

use super::*;

/// A connected server/client pair over loopback TCP (no HTTP handshake).
async fn pair() -> (Conn, Conn) {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let accept = tokio::spawn(async move { l.accept().await.unwrap().0 });
    let client = TcpStream::connect(addr).await.unwrap();
    let server = Conn::start(
        Writer::new(Role::Server),
        Some(accept.await.unwrap()),
        None,
        vec![],
    );
    let client = Conn::start(Writer::new(Role::Client), Some(client), None, Vec::new());
    (server, client)
}

/// A server connection whose peer is a raw socket that never reads, and that peer.
async fn unread_pair() -> (Conn, TcpStream) {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let accept = tokio::spawn(async move { l.accept().await.unwrap().0 });
    let peer = TcpStream::connect(addr).await.unwrap();
    let server = Conn::start(
        Writer::new(Role::Server),
        Some(accept.await.unwrap()),
        None,
        vec![],
    );
    (server, peer)
}

/// What a `send` does: queue now, then wait for room if more than HIGH_WATER is queued.
async fn send(c: &Conn, op: Op, payload: &[u8]) -> io::Result<()> {
    if c.tx.enqueue(op, payload)? {
        c.tx.room().await
    } else {
        Ok(())
    }
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

#[test]
fn sends_keep_call_order_past_the_high_water_mark() {
    rt().block_on(async {
        let (server, client) = pair().await;
        // The first send leaves more than HIGH_WATER queued (it would have waited before
        // queueing); the second must still go out after it.
        let big = vec![b'x'; HIGH_WATER + 1];
        assert!(server.tx.enqueue(Op::Binary, &big).unwrap());
        assert!(server.tx.enqueue(Op::Text, b"after").unwrap());
        let first = client.receive().await.unwrap();
        assert_eq!(
            (first.kind, unsafe { first.data.as_bytes() }.len()),
            (2, big.len())
        );
        assert_eq!(text(&client.receive().await.unwrap()), "after");
        server.tx.room().await.unwrap();
    });
}

#[test]
fn frames_queued_before_the_writer_starts_go_out_in_order() {
    rt().block_on(async {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        let accept = tokio::spawn(async move { l.accept().await.unwrap().0 });
        let peer = TcpStream::connect(addr).await.unwrap();
        // A server-side queue before its upgrade completed.
        let tx = Writer::new(Role::Server);
        tx.enqueue(Op::Text, b"a").unwrap();
        tx.enqueue(Op::Text, b"b").unwrap();
        let _server = Conn::start(tx, Some(accept.await.unwrap()), None, vec![]);
        let client = Conn::start(Writer::new(Role::Client), Some(peer), None, vec![]);
        assert_eq!(text(&client.receive().await.unwrap()), "a");
        assert_eq!(text(&client.receive().await.unwrap()), "b");
    });
}

#[test]
fn close_and_drop_are_bounded_when_the_peer_does_not_read() {
    rt().block_on(async {
        let (server, _peer) = unread_pair().await;
        // Far more than the socket buffers take: the writer blocks in a write.
        for _ in 0..64 {
            let _ = server.tx.enqueue(Op::Binary, &vec![0u8; 256 * 1024]);
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let t = std::time::Instant::now();
        server.close(1000, "").await.unwrap();
        assert!(
            t.elapsed() < CLOSE_FLUSH * 4,
            "close took {:?}",
            t.elapsed()
        );
        // Dropped with data still queued: the writer gives up and lets the socket go.
        let (server, _peer) = unread_pair().await;
        for _ in 0..64 {
            let _ = server.tx.enqueue(Op::Binary, &vec![0u8; 256 * 1024]);
        }
        let tx = server.tx.clone();
        drop(server);
        let t = std::time::Instant::now();
        while Arc::strong_count(&tx) > 1 {
            assert!(
                t.elapsed() < CLOSE_FLUSH * 4,
                "the writer outlived its connection"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    });
}

#[test]
fn invalid_utf8_fails_at_its_fragment() {
    rt().block_on(async {
        let (server, client) = pair().await;
        // A first fragment with an invalid byte; no final fragment ever follows.
        let mut f = Vec::new();
        frame::encode(&mut f, Op::Text, b"ok \xff", true);
        f[0] = 0x01; // FIN clear, text
        client.tx.lock().buf.extend_from_slice(&f);
        client.tx.ready.notify_one();
        match server.receive().await {
            Err(e) => assert!(e.to_string().contains("UTF-8"), "{e}"),
            Ok(m) => panic!("expected an error, got kind {}", m.kind),
        }
        // A character split across fragments is fine.
        let (server, client) = pair().await;
        let euro = "€".as_bytes(); // 3 bytes
        for (fin, op, part) in [(false, 1u8, &euro[..1]), (true, 0, &euro[1..])] {
            let mut f = Vec::new();
            frame::encode(&mut f, Op::Text, part, true);
            f[0] = if fin { 0x80 } else { 0 } | op;
            client.tx.lock().buf.extend_from_slice(&f);
        }
        client.tx.ready.notify_one();
        assert_eq!(text(&server.receive().await.unwrap()), "€");
    });
}

#[test]
fn invalid_close_codes_are_rejected() {
    for code in [0u32, 999, 1004, 1005, 1006, 1015, 2000, 2999, 5000, 70000] {
        // SAFETY: a null handle and a valid string; the leaf is complete and freed below.
        unsafe {
            let reason = VeltStr::from_bytes(b"");
            let f = velt_rt_ws_close(WsHandle::NULL, code, &reason);
            let out =
                &*((f as *const u8).add(crate::task::FUT_RESULT_OFFSET) as *const IoResult<()>);
            let ready = ((*f).poll)(f, std::ptr::null_mut());
            assert_eq!(ready, crate::task::READY);
            assert_eq!(out.err.code, code::INVALID_INPUT, "close code {code}");
            ((*f).drop)(f);
        }
    }
    for code in [1000, 1001, 1003, 1007, 1014, 3000, 4999] {
        assert!(valid_close_code(code));
    }
}

fn text(m: &VeltWsMessage) -> String {
    // SAFETY: the message owns its text.
    String::from_utf8(unsafe { m.text.as_bytes() }.to_vec()).unwrap()
}

#[test]
fn queued_sends_arrive_in_order_and_close_ends_the_stream() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let (server, client) = pair().await;
        for i in 0..1000 {
            send(&server, Op::Text, format!("m{i}").as_bytes())
                .await
                .unwrap();
        }
        for i in 0..1000 {
            let m = client.receive().await.unwrap();
            assert_eq!((m.kind, text(&m)), (1, format!("m{i}")));
        }
        // Client to server (masked), then a binary message larger than one read.
        send(&client, Op::Text, "héllo ✓".as_bytes()).await.unwrap();
        let big: Vec<u8> = (0..200_000u32).map(|i| i as u8).collect();
        send(&client, Op::Binary, &big).await.unwrap();
        let m = server.receive().await.unwrap();
        assert_eq!((m.kind, text(&m).as_str()), (1, "héllo ✓"));
        let m = server.receive().await.unwrap();
        // SAFETY: the message owns its data.
        assert_eq!((m.kind, unsafe { m.data.as_bytes() }), (2, &big[..]));
        server.close(1000, "bye").await.unwrap();
        assert_eq!(client.receive().await.unwrap().kind, 0);
        assert_eq!(client.receive().await.unwrap().kind, 0);
        assert!(send(&server, Op::Text, b"late").await.is_err());
    });
}

#[test]
fn close_reasons_are_cut_at_a_character_boundary() {
    assert_eq!(close_reason("bye"), "bye");
    let long = "é".repeat(100); // 200 bytes: byte 123 is inside a character
    let cut = close_reason(&long);
    assert_eq!(cut.len(), 122);
    assert!(cut.chars().all(|c| c == 'é'));
    assert_eq!(close_reason(&"x".repeat(200)).len(), 123);
}

#[test]
fn pings_are_answered_and_fragments_joined() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let (server, client) = pair().await;
        // Raw frames from the client: a fragmented text with a ping in between.
        let mut raw = Vec::new();
        for (fin, op, payload) in [
            (false, 1u8, &b"frag"[..]),
            (true, 9, b"pp"),
            (false, 0, b"men"),
            (true, 0, b"ted"),
        ] {
            let mut f = Vec::new();
            frame::encode(&mut f, Op::Binary, payload, true);
            f[0] = if fin { 0x80 } else { 0 } | op;
            raw.extend_from_slice(&f);
        }
        {
            let mut q = client.tx.lock();
            q.buf.extend_from_slice(&raw);
        }
        client.tx.ready.notify_one();
        let m = server.receive().await.unwrap();
        assert_eq!((m.kind, text(&m).as_str()), (1, "fragmented"));
        // The server answered the ping with a pong the client skips; then it closes.
        server.close(1000, "").await.unwrap();
        assert_eq!(client.receive().await.unwrap().kind, 0);
    });
}
