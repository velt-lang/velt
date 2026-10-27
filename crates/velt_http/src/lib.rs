//! Minimal HTTP/1.1 for the developer tools: [`message`] (requests, responses, wire format),
//! [`server`] (a thread-per-connection server with a clean shutdown) and [`client`] (one
//! request per connection over `std::net`; `https://` with rustls, configured by [`tls`]).
//!
//! Every connection carries exactly one request (`Connection: close`), request bodies are sized
//! by `Content-Length` and capped by the server, and the client also reads a response body in
//! chunked transfer encoding (or up to the end of the connection, without either).

pub mod client;
pub mod message;
pub mod server;
pub mod tls;

pub use client::{fetch, fetch_within, is_tls_or_loopback, url_host, Limits, UrlHost};
pub use message::{Request, Response};
pub use server::{serve, Handler, Server};
