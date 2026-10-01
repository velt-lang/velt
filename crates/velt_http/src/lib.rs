//! Minimal HTTP/1.1 for the developer tools: [`message`] (requests, responses, wire format),
//! [`server`] (a thread-per-connection server with a clean shutdown) and [`client`] (one
//! request per connection over `std::net`; `https://` through the system `curl`).
//!
//! Every connection carries exactly one request (`Connection: close`), bodies are sized by
//! `Content-Length` (no chunked encoding), and request bodies are capped by the server.

pub mod client;
pub mod message;
pub mod server;

pub use client::fetch;
pub use message::{Request, Response};
pub use server::{serve, Handler, Server};
