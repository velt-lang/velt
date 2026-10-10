//! Side-by-side toolchain versions (#948, docs/tooling/platforms.md "Toolchain versions").
//!
//! A machine keeps each velt version in its own prefix, `<root>/toolchains/<version>/`, beside
//! the launcher `<root>/bin/velt`, which runs the version a package asks for:
//!
//! - [`requirement`]: the `velt` field of `package.vlt` (`"0.1"`, `"=0.1.3"`) and which version
//!   it selects;
//! - [`pin`]: reading that field without the rest of the manifest (the launcher reads it on
//!   every command; vpm checks the whole manifest);
//! - [`layout`]: `<root>`, its installed versions, linked prefixes and the default;
//! - [`release`]: the published versions and installing one from a release;
//! - [`install`]: downloading, verifying and unpacking archives (also `velt target add`'s);
//! - [`signature`]: checking a release's `SHA256SUMS` against the release key.

pub mod install;
pub mod layout;
pub mod pin;
pub mod release;
pub mod requirement;
pub mod signature;

pub use layout::{Root, Toolchain};
pub use requirement::Requirement;
pub use semver::Version;
