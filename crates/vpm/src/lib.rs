//! vpm — the Velt package manager library, used by the `velt` CLI.
//! Manifest format is a contract: docs/internals/contracts/velt_toml.md.
//!
//! - [`manifest`]: `velt.toml` model ([`paths`]: its import aliases); [`edit`]: format-preserving
//!   `velt add`; [`scaffold`]: `velt new`. [`manifest::read`] reads `package.vlt`, which replaces
//!   `velt.toml` (docs/internals/design/package-manifest.md).
//! - [`registry`]: the registry (`publish`, index), local or remote ([`remote`], packages as
//!   [`archive`]s over HTTP); [`cache`]: verified extraction.
//! - [`resolve`]: semver resolution with backtracking; [`lockfile`]: `velt.lock`.
//! - [`install`]: resolve + lock + fetch → [`PackageGraph`] for the compiler's module loader.

pub mod archive;
pub mod cache;
pub mod contents;
pub mod edit;
pub mod graph;
pub mod install;
pub mod locations;
pub mod lockfile;
pub mod manifest;
pub mod paths;
pub mod registry;
pub mod relpath;
pub mod remote;
pub mod resolve;
pub mod scaffold;

pub use graph::PackageGraph;
pub use install::{install, InstallOptions, Installed};
pub use locations::Locations;
pub use manifest::{Dependency, DetailedDependency, Manifest, Package};
