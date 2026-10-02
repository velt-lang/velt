//! In-process protocol tests: a scripted client ([`client`]) talks JSON-RPC to the server, which
//! loads programs through a small relative-imports-only [`loader`].

mod assists;
mod client;
mod jsx;
mod loader;
mod modules;
mod navigation;
mod protocol;
mod quick_fixes;
mod refactor;
