// Server for the fetch client benchmark (bench/http/fetch/README.md): /small (13 bytes),
// /big (100 MB), /json (a 1 MB JSON array of users). Usage: server [port] (default 18080).
use bytes::Bytes;
use http_body_util::Full;
use hyper::{server::conn::http1, service::service_fn, Request, Response};
use hyper_util::rt::TokioIo;
use std::convert::Infallible;
use std::sync::OnceLock;
use tokio::net::TcpListener;

fn json_body() -> Bytes {
    static B: OnceLock<Bytes> = OnceLock::new();
    B.get_or_init(|| {
        let mut s = String::from("[");
        let mut i = 0;
        while s.len() < 1_000_000 {
            if i > 0 { s.push(','); }
            s.push_str(&format!("{{\"id\":{i},\"name\":\"user{i}\",\"email\":\"user{i}@example.com\",\"active\":{},\"score\":{}.5}}", i % 2 == 0, i * 7));
            i += 1;
        }
        s.push(']');
        Bytes::from(s)
    }).clone()
}

fn big() -> Bytes {
    static B: OnceLock<Bytes> = OnceLock::new();
    B.get_or_init(|| Bytes::from(vec![b'x'; 100 * 1024 * 1024])).clone()
}

async fn handle(req: Request<hyper::body::Incoming>) -> Result<Response<Full<Bytes>>, Infallible> {
    let (body, ct) = match req.uri().path() {
        "/big" => (big(), "application/octet-stream"),
        "/json" => (json_body(), "application/json"),
        _ => (Bytes::from_static(b"Hello, World!"), "text/plain"),
    };
    Ok(Response::builder().header("content-type", ct).body(Full::new(body)).unwrap())
}

#[tokio::main]
async fn main() {
    let port: u16 = std::env::args().nth(1).map(|p| p.parse().unwrap()).unwrap_or(18080);
    let l = TcpListener::bind(("127.0.0.1", port)).await.unwrap();
    json_body(); big();
    println!("listening {port}");
    loop {
        let (s, _) = l.accept().await.unwrap();
        s.set_nodelay(true).ok();
        tokio::spawn(async move {
            let _ = http1::Builder::new().serve_connection(TokioIo::new(s), service_fn(handle)).await;
        });
    }
}
