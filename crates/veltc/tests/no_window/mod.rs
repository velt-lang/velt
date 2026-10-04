//! Child processes of `veltc`'s tests start without a console window of their own: the shared
//! `command()` (tests/common/command.rs).

#[path = "../../../../tests/common/command.rs"]
mod command;

pub use command::command;
