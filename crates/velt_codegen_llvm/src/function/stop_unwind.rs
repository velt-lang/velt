//! The functions C calls (`vir::Function::stops_unwind`, `@export("c_name")`): a panic that
//! unwinds into one aborts the process, as clang does for a `noexcept` function and Rust for an
//! `extern "C"` one. Each call in it that can unwind is an `invoke` whose unwind edge goes to one
//! landing pad calling `abort`, with the personality function of the runtime's panics: Rust's
//! (`rust_eh_personality`, the runtime defines it) for the Itanium unwinder of Linux and macOS,
//! `__CxxFrameHandler3` (a `cleanuppad`) for Windows' SEH panics.
//!
//! Missing unwind information alone would not do: LLVM writes it anyway with `-g` (its CFI goes to
//! `.eh_frame` as the other functions'), and a call in tail position becomes a jump, which leaves
//! no frame of the export to stop at. An `invoke` is never a tail call, and its landing pad is
//! reached in the unwinder's second phase, after the first one found the `catchPanic` above.

use crate::target::{Os, Target};

use super::Emitter;

/// The landing pad's label.
const PAD: &str = "velt.abort";

/// How a function C calls stops an unwind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Pad {
    /// Itanium: `landingpad … cleanup` with Rust's personality.
    Landing,
    /// Windows: `cleanuppad` with the MSVC C++ personality Rust panics use.
    Cleanup,
}

impl Pad {
    /// The pad `function` needs, if it is one C calls.
    pub(super) fn of(function: &velt_vir::vir::Function, target: &Target) -> Option<Pad> {
        function.stops_unwind.then_some(match target.os {
            Os::Windows => Pad::Cleanup,
            _ => Pad::Landing,
        })
    }

    fn personality(self) -> &'static str {
        match self {
            Pad::Landing => "@rust_eh_personality",
            Pad::Cleanup => "@__CxxFrameHandler3",
        }
    }
}

impl Emitter<'_> {
    /// Whether a call of `target` becomes an `invoke` (LLVM's intrinsics stay calls: most can't be
    /// invoked, and none unwinds).
    pub(super) fn invokes(&self, target: &str) -> bool {
        self.pad.is_some() && !target.starts_with("@llvm.")
    }

    /// `invoke` in place of the call `text` (`call <ret> <target>(<args>)[ attrs]`), continuing
    /// at a fresh label, which the caller places after it.
    pub(super) fn invoke_text(&mut self, text: &str) -> (String, String) {
        let label = format!("inv{}", self.invokes_made);
        self.invokes_made += 1;
        let rest = text.strip_prefix("call ").unwrap_or(text);
        (
            format!("invoke {rest} to label %{label} unwind label %{PAD}"),
            label,
        )
    }

    /// The landing pad, after the function's blocks, and ` personality …` for its `define` line;
    /// empty for a function that isn't one C calls or has no `invoke` (its calls were inlined).
    pub(super) fn finish_pad(&mut self) -> String {
        let Some(pad) = self.pad.filter(|_| self.invokes_made > 0) else {
            return String::new();
        };
        let p = pad.personality();
        self.intrinsics.need(format!("declare i32 {p}(...)"));
        self.intrinsics
            .need("declare void @abort() noreturn nounwind".to_string());
        self.label(PAD);
        match pad {
            Pad::Landing => {
                self.line("%velt.lp = landingpad { ptr, i32 } cleanup".into());
                self.line("call void @abort() noreturn nounwind".into());
            }
            Pad::Cleanup => {
                self.line("%velt.cp = cleanuppad within none []".into());
                self.line(
                    "call void @abort() noreturn nounwind [ \"funclet\"(token %velt.cp) ]".into(),
                );
            }
        }
        self.line("unreachable".into());
        format!(" personality ptr {p}")
    }
}
