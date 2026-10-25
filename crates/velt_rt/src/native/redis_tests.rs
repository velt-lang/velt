//! std/redis's multiplexed connection (`velt_std_redis::multiplex`) on this runtime, against a
//! fake server: replies in order, a lost connection reopened, giving up after the reconnect
//! window. (The driver's crate has no runtime of its own to run them on.)

use super::aio_tests::{init, run};
use std::io::{Read, Write};
use std::time::Duration;
use velt_std_redis::connect::Endpoint;
use velt_std_redis::multiplex::Conn;
use velt_std_redis::resp::Value;
use velt_std_redis::url;

/// A fake server that answers each `PING` with `:n` (its count so far on that connection),
/// closes a connection after `limit` commands, and accepts `connections` connections (it
/// stops listening when it accepts the last one).
fn fake_server(limit: usize, connections: usize) -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let mut l = Some(l);
        for i in 0..connections {
            let (mut s, _) = l.as_ref().unwrap().accept().unwrap();
            if i + 1 == connections {
                l = None;
            }
            let (mut seen, mut buf) = (0, vec![0u8; 4096]);
            while seen < limit {
                let n = s.read(&mut buf).unwrap();
                let pings = buf[..n].windows(4).filter(|w| w == b"PING").count();
                let mut out = String::new();
                for _ in 0..pings.min(limit - seen) {
                    seen += 1;
                    out.push_str(&format!(":{seen}\r\n"));
                }
                s.write_all(out.as_bytes()).unwrap();
            }
        }
        drop(l);
    });
    port
}

fn ping(n: usize) -> Vec<u8> {
    let mut p = vec![];
    for _ in 0..n {
        velt_std_redis::resp::encode_command(&["PING"], &mut p);
    }
    p
}

/// Wait until `conn` saw `n` connections end (a hang guard of a minute, not a time limit).
async fn lost(conn: &Conn, n: u64) {
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while conn.connections_lost() < n {
        assert!(
            std::time::Instant::now() < deadline,
            "the connection never ended"
        );
        velt_native::time::sleep(Duration::from_millis(1)).await;
    }
}

async fn open(port: u16) -> Conn {
    let target = url::parse(&format!("redis://127.0.0.1:{port}")).unwrap();
    let mut ep = Endpoint::new(target, vec![]);
    // Giving up is tested without waiting the real window.
    ep.reconnect_window = Duration::from_millis(300);
    Conn::open(ep).await.unwrap()
}

#[test]
fn replies_arrive_in_order_and_a_lost_connection_is_reopened() {
    init();
    let port = fake_server(3, 2);
    run(async move {
        let conn = open(port).await;
        assert_eq!(conn.send(ping(1), 1).await, Ok(vec![Value::Int(1)]));
        assert_eq!(
            conn.send(ping(2), 2).await,
            Ok(vec![Value::Int(2), Value::Int(3)])
        );
        // The server closed the first connection; the next command gets a new one.
        lost(&conn, 1).await;
        assert_eq!(conn.send(ping(1), 1).await, Ok(vec![Value::Int(1)]));
    });
}

#[test]
fn requests_fail_once_reconnecting_gives_up() {
    init();
    let port = fake_server(1, 1);
    run(async move {
        let conn = open(port).await;
        assert_eq!(conn.send(ping(1), 1).await, Ok(vec![Value::Int(1)]));
        lost(&conn, 1).await;
        let e = conn.send(ping(1), 1).await.unwrap_err();
        assert_eq!(e.code, velt_native::code::CONNECTION_REFUSED);
    });
}
