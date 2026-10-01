//! Debug metadata (`!dbg`): emitted exactly when the VIR carries source locations
//! (`vir::Program::files` non-empty; the driver keeps them for debug builds and `-g`).
//!
//! One `DICompileUnit`, a `DIFile` per source file, a `DISubprogram` per defined function (also
//! for compiler-generated ones, at line 0, so every call site inside a function with debug info
//! has a location, as the LLVM verifier demands for inlinable calls) and a `DILocation` per
//! distinct statement location. Statements that came from another file (code inlined by
//! `velt_opt`) are scoped in a `DILexicalBlockFile` of that file. The module flags select
//! CodeView on Windows (turned into a PDB by `link /DEBUG`) and DWARF 4 elsewhere.

use std::collections::HashMap;
use std::fmt::Write;

use velt_vir::vir::{self, Linkage, SrcLoc};

use crate::target::{Os, Target};

/// Module-wide debug metadata being built.
pub(crate) struct DebugInfo {
    /// Metadata definitions (`!N = …`), in creation order.
    nodes: String,
    next: u32,
    compile_unit: u32,
    /// `DIFile` per `vir::Program::files` index.
    files: Vec<u32>,
    subroutine_type: u32,
}

/// Debug state of the function being translated.
pub(crate) struct FnDebug {
    subprogram: u32,
    file: u32,
    /// Scope for statements of another file, per file index.
    file_scopes: HashMap<u32, u32>,
    locations: HashMap<(u32, u32, u32), u32>,
}

impl DebugInfo {
    /// Debug metadata for `program`, or `None` when it has no source locations.
    pub(crate) fn new(program: &vir::Program, optimized: bool) -> Option<DebugInfo> {
        if program.files.is_empty() {
            return None;
        }
        let mut d = DebugInfo {
            nodes: String::new(),
            next: 0,
            compile_unit: 0,
            files: vec![],
            subroutine_type: 0,
        };
        let cwd = std::env::current_dir()
            .map(|p| p.display().to_string().replace('\\', "/"))
            .unwrap_or_default();
        for path in &program.files {
            let (name, dir) = file_and_directory(path, &cwd);
            let id = d.node(format!(
                "!DIFile(filename: {}, directory: {})",
                quote(&name),
                quote(&dir)
            ));
            d.files.push(id);
        }
        d.compile_unit = d.node(format!(
            "distinct !DICompileUnit(language: DW_LANG_C, file: !{}, producer: \"velt\", \
             isOptimized: {optimized}, runtimeVersion: 0, emissionKind: FullDebug)",
            d.files[0]
        ));
        d.subroutine_type = d.node("!DISubroutineType(types: !{})".into());
        Some(d)
    }

    fn node(&mut self, text: String) -> u32 {
        let id = self.next;
        self.next += 1;
        let _ = writeln!(self.nodes, "!{id} = {text}");
        id
    }

    /// The `DISubprogram` of `function` (at its first known location, else line 0 of the
    /// first file) and the per-function state.
    pub(crate) fn function(&mut self, function: &vir::Function) -> FnDebug {
        let at = function.first_loc();
        let file_index = at.map_or(0, |l| l.file) as usize;
        let file = self.files.get(file_index).copied().unwrap_or(self.files[0]);
        let line = at.map_or(0, |l| l.line);
        let local = match function.linkage {
            Linkage::Internal => " | DISPFlagLocalToUnit",
            Linkage::Export => "",
        };
        let name = velt_vir::mangle::demangle(&function.symbol);
        let subprogram = self.node(format!(
            "distinct !DISubprogram(name: {}, linkageName: {}, scope: !{file}, file: !{file}, \
             line: {line}, type: !{}, scopeLine: {line}, flags: DIFlagPrototyped, \
             spFlags: DISPFlagDefinition{local}, unit: !{})",
            quote(&name),
            quote(&function.symbol),
            self.subroutine_type,
            self.compile_unit
        ));
        FnDebug {
            subprogram,
            file: file_index as u32,
            file_scopes: HashMap::new(),
            locations: HashMap::new(),
        }
    }

    /// The `DILocation` for `at` (line 0 of the function when unknown).
    pub(crate) fn location(&mut self, f: &mut FnDebug, at: Option<SrcLoc>) -> u32 {
        let (file, line, col) = at.map_or((f.file, 0, 0), |l| (l.file, l.line, l.col));
        if let Some(&id) = f.locations.get(&(file, line, col)) {
            return id;
        }
        let scope = self.scope(f, file);
        let id = self.node(format!(
            "!DILocation(line: {line}, column: {col}, scope: !{scope})"
        ));
        f.locations.insert((file, line, col), id);
        id
    }

    fn scope(&mut self, f: &mut FnDebug, file: u32) -> u32 {
        if file == f.file {
            return f.subprogram;
        }
        if let Some(&s) = f.file_scopes.get(&file) {
            return s;
        }
        let di_file = self
            .files
            .get(file as usize)
            .copied()
            .unwrap_or(self.files[0]);
        let s = self.node(format!(
            "!DILexicalBlockFile(scope: !{}, file: !{di_file}, discriminator: 0)",
            f.subprogram
        ));
        f.file_scopes.insert(file, s);
        s
    }

    /// ` !dbg !N` suffix for the `define` line.
    pub(crate) fn define_suffix(f: &FnDebug) -> String {
        format!(" !dbg !{}", f.subprogram)
    }

    /// Named metadata, module flags and every node, for the end of the module.
    pub(crate) fn finish(mut self, target: &Target) -> String {
        let version = self.node("!{i32 2, !\"Debug Info Version\", i32 3}".into());
        let format = match target.os {
            Os::Windows => self.node("!{i32 2, !\"CodeView\", i32 1}".into()),
            Os::Darwin | Os::Linux | Os::Wasi | Os::WasmBrowser => {
                self.node("!{i32 7, !\"Dwarf Version\", i32 4}".into())
            }
        };
        format!(
            "\n!llvm.dbg.cu = !{{!{}}}\n!llvm.module.flags = !{{!{version}, !{format}}}\n{}",
            self.compile_unit, self.nodes
        )
    }
}

/// `(filename, directory)` for a `DIFile`: relative paths are relative to the current
/// directory (where the compiler ran), absolute ones stand alone.
fn file_and_directory(path: &str, cwd: &str) -> (String, String) {
    let absolute = path.starts_with('/') || path.get(1..3).is_some_and(|s| s == ":/");
    if absolute {
        match path.rsplit_once('/') {
            Some((dir, name)) => (name.to_string(), dir.to_string()),
            None => (path.to_string(), String::new()),
        }
    } else {
        (path.to_string(), cwd.to_string())
    }
}

/// A metadata string literal: printable ASCII except `"` and `\` verbatim, the rest `\XX`.
fn quote(s: &str) -> String {
    let mut out = String::from("\"");
    for b in s.bytes() {
        if (0x20..0x7f).contains(&b) && b != b'"' && b != b'\\' {
            out.push(b as char);
        } else {
            let _ = write!(out, "\\{b:02X}");
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting_and_paths() {
        assert_eq!(quote("a\"b\\c\u{e9}"), "\"a\\22b\\5Cc\\C3\\A9\"");
        assert_eq!(
            file_and_directory("C:/x/std/prelude/result.vlt", "C:/w"),
            ("result.vlt".into(), "C:/x/std/prelude".into())
        );
        assert_eq!(
            file_and_directory("/usr/lib/a.vlt", "/w"),
            ("a.vlt".into(), "/usr/lib".into())
        );
        assert_eq!(
            file_and_directory("examples/foo.vlt", "C:/w"),
            ("examples/foo.vlt".into(), "C:/w".into())
        );
    }
}
