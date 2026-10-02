//! Source locations for lowering (`velt_vir::lower_with`): spans → 1-based `SrcLoc`s via
//! precomputed line tables, the per-function "current location" that every emitted statement
//! records, and the ` at <path>:<line>:<col>` suffix of compiler-emitted panics.
//!
//! Functions defined in the standard library that panic on behalf of their caller (`unwrap`,
//! `assert*`, …) *track the caller*, like Rust's `#[track_caller]`: each direct call site gets
//! its own instance (`Work::Tracked`) whose panics report the call site (see track_caller.rs).

use velt_common::{SourceMap, Span};

use super::FnLower;
use crate::vir::{Operand, SrcLoc};

/// Line tables of every source file, plus the paths reported in `vir::Program::files`.
pub(super) struct LocMap {
    /// Display path per `FileId` (forward slashes).
    pub files: Vec<String>,
    /// Byte offset of the start of each line, per file.
    line_starts: Vec<Vec<u32>>,
    /// Paths (per `FileId`) that belong to the standard library.
    std_files: Vec<bool>,
}

impl LocMap {
    /// Tables for every file of `sm`. `std_root` marks standard-library files.
    pub(super) fn new(sm: &SourceMap, std_root: Option<&std::path::Path>) -> LocMap {
        let mut files = vec![];
        let mut line_starts = vec![];
        let mut std_files = vec![];
        for (_, f) in sm.files() {
            files.push(f.path.display().to_string().replace('\\', "/"));
            let starts = std::iter::once(0)
                .chain(
                    f.src
                        .bytes()
                        .enumerate()
                        .filter(|(_, b)| *b == b'\n')
                        .map(|(i, _)| i as u32 + 1),
                )
                .collect();
            line_starts.push(starts);
            std_files.push(std_root.is_some_and(|r| f.path.starts_with(r)));
        }
        LocMap {
            files,
            line_starts,
            std_files,
        }
    }

    /// Location of the start of `span`; `None` for unknown files.
    pub(super) fn loc(&self, span: Span) -> Option<SrcLoc> {
        let starts = self.line_starts.get(span.file.0 as usize)?;
        let line = starts.partition_point(|&s| s <= span.lo).max(1);
        let col = span.lo - starts[line - 1] + 1;
        Some(SrcLoc {
            file: span.file.0,
            line: line as u32,
            col,
        })
    }

    /// Whether `span` lies in a standard-library file.
    pub(super) fn is_std(&self, span: Span) -> bool {
        self.std_files
            .get(span.file.0 as usize)
            .copied()
            .unwrap_or(false)
    }

    /// Whether `loc` lies in a standard-library file.
    pub(super) fn is_std_loc(&self, loc: SrcLoc) -> bool {
        self.std_files
            .get(loc.file as usize)
            .copied()
            .unwrap_or(false)
    }

    /// `path:line:col`.
    pub(super) fn describe(&self, loc: SrcLoc) -> String {
        let path = self
            .files
            .get(loc.file as usize)
            .map_or("?", |s| s.as_str());
        format!("{path}:{}:{}", loc.line, loc.col)
    }
}

impl FnLower<'_, '_> {
    /// `Intrinsic::SourceLocation`: `"path:line:col"` of `span` as a string constant (the user's
    /// file as it was given to the compiler).
    pub(super) fn source_location(&mut self, span: Span) -> Operand {
        let text = self
            .cx
            .locs
            .as_ref()
            .and_then(|m| m.loc(span).map(|l| m.describe(l)))
            .unwrap_or_else(|| "an unknown location".into());
        self.str_lit(&text)
    }

    /// Make `span` the location of the statements emitted from now on; returns the previous
    /// location for [`FnLower::restore_loc`]. Empty spans (compiler-synthesized nodes,
    /// `Span::DUMMY`) keep the enclosing location.
    pub(super) fn enter_span(&mut self, span: Span) -> Option<SrcLoc> {
        let prev = self.loc;
        if span.hi <= span.lo {
            return prev;
        }
        if let Some(l) = self.cx.locs.as_ref().and_then(|m| m.loc(span)) {
            self.loc = Some(l);
        }
        prev
    }

    pub(super) fn restore_loc(&mut self, prev: Option<SrcLoc>) {
        self.loc = prev;
    }

    /// The location a panic raised here reports: the tracked caller's call site, else the
    /// current location.
    pub(super) fn panic_loc(&self) -> Option<SrcLoc> {
        self.caller_loc.or(self.loc)
    }

    /// Whether the code being lowered now is standard-library code (not a caller-tracking
    /// instance, whose location is its user call site).
    pub(super) fn in_std(&self) -> bool {
        match (self.cx.locs.as_ref(), self.caller_loc, self.loc) {
            (Some(m), None, Some(l)) => m.is_std_loc(l),
            _ => false,
        }
    }

    /// ` at <path>:<line>:<col>` for a panic raised here (empty without location info).
    pub(super) fn panic_suffix(&self) -> String {
        match (self.cx.locs.as_ref(), self.panic_loc()) {
            (Some(m), Some(l)) => format!(" at {}", m.describe(l)),
            _ => String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use velt_common::FileId;

    #[test]
    fn line_and_column() {
        let mut sm = SourceMap::new();
        sm.add("a.vlt", "ab\ncd\n\nxyz");
        let m = LocMap::new(&sm, None);
        let at = |lo| m.loc(Span::new(FileId(0), lo, lo)).unwrap();
        assert_eq!((at(0).line, at(0).col), (1, 1));
        assert_eq!((at(1).line, at(1).col), (1, 2));
        assert_eq!((at(3).line, at(3).col), (2, 1));
        assert_eq!((at(6).line, at(6).col), (3, 1));
        assert_eq!((at(9).line, at(9).col), (4, 3));
        for lo in 0..10 {
            let (l, c) = sm.line_col(FileId(0), lo);
            assert_eq!((at(lo).line as usize, at(lo).col as usize), (l, c), "{lo}");
        }
        assert!(m.loc(Span::new(FileId(7), 0, 0)).is_none());
        assert_eq!(m.describe(at(9)), "a.vlt:4:3");
    }
}
