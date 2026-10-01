//! A thread-per-connection HTTP server: each connection reads one request, runs the handler
//! and writes its response. [`Server`] runs in the background and stops cleanly (tests, tools
//! that serve while doing something else); [`serve`] blocks (CLI commands).

use std::io::{BufReader, Read};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use crate::message::{read_request, write_response, Request, Response};

/// Handles one request.
pub type Handler = Arc<dyn Fn(Request) -> Response + Send + Sync>;

/// How long a connection may take to send its request.
const READ_TIMEOUT: Duration = Duration::from_secs(30);
/// How long, and how much, unread request input is drained after the response.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(2);
const DRAIN_LIMIT: u64 = 64 << 20;

/// A server running on a background thread.
pub struct Server {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Server {
    /// Serve `listener` in the background; request bodies are capped at `max_body` bytes.
    pub fn start(
        listener: TcpListener,
        handler: Handler,
        max_body: usize,
    ) -> std::io::Result<Server> {
        let addr = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::Builder::new()
            .name("velt-http-accept".into())
            .spawn(move || accept_loop(&listener, &handler, max_body, &flag))?;
        Ok(Server {
            addr,
            stop,
            thread: Some(thread),
        })
    }

    /// The bound address (the real port when bound to port 0).
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Stop accepting and wait for the accept thread (in-flight requests finish on their own).
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wake the blocking accept with a connection of our own.
        let _ = TcpStream::connect(self.addr);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if self.thread.is_some() {
            self.shutdown();
        }
    }
}

/// Serve `listener` on this thread until the process ends.
pub fn serve(listener: &TcpListener, handler: Handler, max_body: usize) {
    accept_loop(listener, &handler, max_body, &AtomicBool::new(false));
}

fn accept_loop(listener: &TcpListener, handler: &Handler, max_body: usize, stop: &AtomicBool) {
    for conn in listener.incoming() {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        let Ok(stream) = conn else { continue };
        let handler = handler.clone();
        let _ = std::thread::Builder::new()
            .name("velt-http-conn".into())
            .spawn(move || handle(stream, &handler, max_body));
    }
}

fn handle(stream: TcpStream, handler: &Handler, max_body: usize) {
    let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
    let Ok(mut writer) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(stream);
    let response = match read_request(&mut reader, max_body) {
        Ok(req) => handler(req),
        Err(e) if e.contains("exceeds") => Response::text(413, e),
        Err(e) => Response::text(400, e),
    };
    let _ = write_response(&mut writer, &response);
    // A rejected request may still be sending its body. Closing with unread input would reset
    // the connection and lose the response, so half-close and drain what is left (bounded).
    let _ = writer.shutdown(Shutdown::Write);
    let _ = reader.get_ref().set_read_timeout(Some(DRAIN_TIMEOUT));
    let _ = std::io::copy(&mut reader.take(DRAIN_LIMIT), &mut std::io::sink());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serves_and_stops() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let handler: Handler = Arc::new(|req: Request| {
            Response::text(
                200,
                format!("{} {} {}", req.method, req.path, req.body.len()),
            )
        });
        let server = Server::start(listener, handler, 16).unwrap();
        let url = format!("http://{}/x", server.addr());
        let resp = crate::fetch("POST", &url, &[], b"hello").unwrap();
        assert_eq!((resp.status, resp.body_text()), (200, "POST /x 5".into()));
        let big = crate::fetch("POST", &url, &[], &[0; 64]).unwrap();
        assert_eq!(big.status, 413);
        server.stop();
    }
}
