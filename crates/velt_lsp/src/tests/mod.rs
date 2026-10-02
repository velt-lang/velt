//! In-process protocol tests: a scripted client ([`client`]) talks JSON-RPC to the server, which
//! loads programs through a small relative-imports-only [`loader`].

mod assists;
mod client;
mod effects;
mod jsx;
mod jsx_more;
mod loader;
mod manifest;
mod modules;
mod navigation;
mod protocol;
mod quick_fixes;
mod refactor;
mod server_features;
