//! In-process protocol tests: a scripted client ([`client`]) talks JSON-RPC to the server, which
//! loads programs through a small relative-imports-only [`loader`].

mod assists;
mod client;
mod default_exports;
mod doc_comments;
mod doc_surfaces;
mod effects;
mod imports;
mod jsx;
mod jsx_attrs;
mod jsx_more;
mod jsx_tags;
mod loader;
mod manifest;
mod manifest_registry;
mod modules;
mod navigation;
mod operators;
mod protocol;
mod quick_fixes;
mod quoted_keys;
mod refactor;
mod server_features;
mod slow;
mod ts_compat;
mod type_value_names;
