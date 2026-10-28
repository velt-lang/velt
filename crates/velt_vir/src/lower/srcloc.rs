//! Source locations for lowering (`velt_vir::lower_with`): spans → 1-based `SrcLoc`s via
//! precomputed line tables, the per-function "current location" that every emitted statement
//! records, and the ` at <path>:<line>:<col>` suffix of compiler-emitted panics.
//!
//! Texts compiled into the program name a standard-library file relative to the std root
//! (`std/json/scan.vlt:12:3`), so they are the same on every machine: code compiled once and
//! shared, such as a cached std object (#1728), must not carry one machine's paths. Debug info
//! keeps the path as loaded, where a debugger finds the file.
//!
//! Functions defined in the standard library that panic on behalf of their caller (`unwrap`,
//! `assert*`, …) *track the caller*, like Rust's `#[track_caller]`: a small one gets an instance
//! per direct call site (`Work::Tracked`) whose panics report the call site, a larger one takes
//! it as a parameter (`ARG_LOC`; see track_caller.rs).

use velt_common::{SourceMap, Span};

use super::FnLower;
use crate::vir::{self, Operand, SrcLoc, Ty};

/// The caller location of a caller-tracking instance called from glue, which has none: its
/// panics report no location (`FnLower::panic_suffix`).
pub(super) const UNKNOWN_LOC: SrcLoc = SrcLoc {
    file: u32::MAX,
    line: 0,
    col: 0,
};

/// The caller location of a caller-tracking instance that takes it as its last parameter
/// (`FnLower::caller_arg`), one instance for all call sites (track_caller.rs).
pub(super) const ARG_LOC: SrcLoc = SrcLoc {
    file: u32::MAX,
    line: 1,
    col: 0,
};

/// Line tables of every source file, plus the paths reported in `vir::Program::files`.
pub(super) struct LocMap {
    /// Display path per `FileId` (forward slashes), for debug info.
    pub files: Vec<String>,
    /// Path per `FileId` in texts the program prints: as `files`, but `std/<path>` for a
    /// standard-library file.
    shown: Vec<String>,
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
        let mut shown = vec![];
        for (_, f) in sm.files() {
            let path = f.path.display().to_string().replace('\\', "/");
            let in_std = std_root.and_then(|r| f.path.strip_prefix(r).ok());
            shown.push(match in_std {
                Some(rel) => format!("std/{}", rel.display().to_string().replace('\\', "/")),
                None => path.clone(),
            });
            files.push(path);
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
            std_files.push(in_std.is_some());
        }
        LocMap {
            files,
            shown,
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

    /// `path:line:col`, a std file's path relative to the std root.
    pub(super) fn describe(&self, loc: SrcLoc) -> String {
        let path = self
            .shown
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

    /// `span` becomes the user code being lowered (`Cx::user_site`) when it is in a user file;
    /// returns the previous one.
    pub(super) fn enter_site(&mut self, span: Span) -> Span {
        let prev = self.cx.user_site;
        if span.hi > span.lo && self.cx.locs.as_ref().is_some_and(|m| !m.is_std(span)) {
            self.cx.user_site = span;
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

    /// ` at <path>:<line>:<col>` for a panic raised here (empty without location info), known
    /// when lowering: not in an instance that takes its caller's location (`panic_at`).
    pub(super) fn panic_suffix(&self) -> String {
        match (self.cx.locs.as_ref(), self.panic_loc()) {
            (Some(_), Some(ARG_LOC)) => {
                super::ice("panic_suffix in an instance that takes its caller's location")
            }
            (Some(m), Some(l)) if l != UNKNOWN_LOC => format!(" at {}", m.describe(l)),
            _ => String::new(),
        }
    }

    /// Where a panic raised here is, as a pointer to the `Str` ` at <path>:<line>:<col>`: the
    /// parameter of an instance that takes its caller's location, else a static one (empty
    /// without location info).
    pub(super) fn panic_at(&mut self) -> Operand {
        if let (Some(ARG_LOC), Some(p)) = (self.panic_loc(), self.caller_arg) {
            return Operand::Copy(vir::Place::local(p));
        }
        let suffix = self.panic_suffix();
        let id = self.cx.static_str_object(&suffix);
        Operand::Const(vir::Const::Static(id), Ty::Ptr)
    }

    /// The text `msg` with ` at …` for a panic raised here, as a pointer to its `Str`: a
    /// constant, or for an instance that takes its caller's location the two joined (the
    /// process ends right after, so the joined text is never freed).
    pub(super) fn panic_text(&mut self, msg: &str) -> Operand {
        let str_agg = Ty::Agg(vir::STR_AGG);
        if self.panic_loc() != Some(ARG_LOC) {
            let text = format!("{msg}{}", self.panic_suffix());
            let lit = self.str_lit(&text);
            return self.operand_addr(lit, str_agg);
        }
        let lit = self.str_lit(msg);
        let head = self.operand_addr(lit, str_agg);
        let at = self.panic_at();
        // Not `concat`: no scope drops the text (a panic block may be built after the body's
        // scopes closed, as the division-by-zero ones are).
        let joined = self.temp(str_agg);
        let out = self.addr(vir::Place::local(joined));
        self.call_rt(super::rt::Rt::StrConcat, vec![head, at, out.clone()], None);
        out
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

    #[test]
    fn std_files_are_shown_relative_to_the_std_root() {
        let root = std::path::Path::new("/opt/velt/std");
        let mut sm = SourceMap::new();
        sm.add("/home/me/app.vlt", "x");
        sm.add(root.join("json").join("scan.vlt"), "\n y");
        let m = LocMap::new(&sm, Some(root));
        let loc = |file, line, col| SrcLoc { file, line, col };
        assert_eq!(m.describe(loc(0, 1, 1)), "/home/me/app.vlt:1:1");
        assert_eq!(m.describe(loc(1, 2, 2)), "std/json/scan.vlt:2:2");
        assert!(m.is_std_loc(loc(1, 2, 2)) && !m.is_std_loc(loc(0, 1, 1)));
        // Debug info keeps the path a debugger opens.
        assert_eq!(m.files[1], "/opt/velt/std/json/scan.vlt");
    }
}
