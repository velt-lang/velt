//! Library side of the `velt` CLI: argument parsing ([`cli`]), module loading ([`loader`]), the
//! compilation pipeline ([`driver`], with [`backend`] selection and the [`link`] step), command
//! execution ([`commands`], incl. vpm and `velt test`)
//! the `velt dev` supervisor and JIT host ([`dev`]), and the `velt playground` server
//! ([`playground`]), terminal colors ([`style`]) and the `velt new` project [`templates`].
//! The binary (`src/main.rs`) is a thin wrapper so tests can drive everything directly.

pub mod backend;
pub mod cli;
pub mod commands;
pub mod dev;
pub mod driver;
pub mod link;
pub mod loader;
pub mod native;
mod numbers_report;
pub mod playground;
pub mod style;
pub mod templates;
