//! In-process protocol tests: a scripted client ([`client`]) talks JSON-RPC to the server, which
//! loads programs through a small relative-imports-only [`loader`].

mod assists;
mod client;
mod effects;
mod imports;
mod jsx;
mod jsx_more;
mod jsx_tags;
mod loader;
mod manifest;
mod manifest_registry;
mod modules;
mod navigation;
mod protocol;
mod quick_fixes;
mod refactor;
mod server_features;
mod ts_compat;
