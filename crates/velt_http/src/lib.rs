//! Minimal HTTP/1.1 for the developer tools: [`message`] (requests, responses, wire format),
//! [`server`] (a thread-per-connection server with a clean shutdown) and [`client`] (one
//! request per connection over `std::net`; `https://` with rustls, configured by [`tls`]).
//!
//! Every connection carries exactly one request (`Connection: close`), bodies are sized by
//! `Content-Length` (no chunked encoding), and request bodies are capped by the server.

pub mod client;
pub mod message;
pub mod server;
pub mod tls;

pub use client::{fetch, fetch_within, is_tls_or_loopback, url_host, Limits, UrlHost};
pub use message::{Request, Response};
pub use server::{serve, Handler, Server};
