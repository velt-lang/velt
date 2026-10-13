//! DWARF line tables for Cranelift code, emitted exactly when the VIR carries source locations
//! (`vir::Program::files`; debug builds and `-g`), like the LLVM backend's `!dbg` metadata.
//!
//! The translator tags every instruction with the location of its VIR statement (a Cranelift
//! `SourceLoc` indexing a per-function table); after compilation, Cranelift reports which code
//! ranges carry which location, and [`FunctionLines`] keeps them as rows. [`build_unit`] turns
//! the functions into one DWARF 4 compile unit: a `DW_TAG_subprogram` per function (name,
//! linkage name, code range, declaration line) and a line program. Debug builds also describe
//! source variables (`vir::LocalDecl::debug`): each has a stack slot keyed by its local
//! (function/mod.rs; its storage, or for a scalar a shadow that every write updates), found in
//! the compiled frame and located relative to the frame pointer, which Cranelift always keeps;
//! `types` builds their DWARF types.
//!
//! - `object`: the sections of ELF, Mach-O and COFF objects, with relocations against the
//!   function symbols;
//! - `jit`: an in-memory ELF image with absolute addresses for `velt dev`'s JIT code, announced
//!   to GDB and LLDB through the GDB JIT interface.

use std::cmp::Reverse;

use cranelift_codegen::gimli::write::{
    Address, AttributeValue, DwarfUnit, Expression, FileId, LineProgram, LineString, Range,
    RangeList, UnitEntryId,
};
use cranelift_codegen::gimli::{self, Encoding, Format, LineEncoding};
use cranelift_codegen::Context;
use cranelift_module::FuncId;
use cranelift_object::object::Architecture;
use velt_vir::vir::{self, SrcLoc};

#[cfg(unix)]
pub(crate) mod jit;
pub(crate) mod object;
mod types;

/// The line information of one compiled function.
pub(crate) struct FunctionLines {
    pub id: FuncId,
    /// Mangled symbol (`DW_AT_linkage_name`).
    pub symbol: String,
    /// Source-level name (`DW_AT_name`).
    pub name: String,
    pub external: bool,
    /// Code size in bytes.
    pub size: u32,
    /// Declaration: the function's first known location.
    pub decl: Option<SrcLoc>,
    /// Rows by ascending code offset: the location of the code from there on, `None` for code
    /// without one (line 0 for debuggers).
    pub rows: Vec<(u32, Option<SrcLoc>)>,
    /// Where the body starts (the end of the prologue).
    pub body: u32,
    /// The source variables, by local index.
    pub vars: Vec<Variable>,
}

/// A source variable of a compiled function: the stack slot of the local holding it.
pub(crate) struct Variable {
    pub name: String,
    pub debug: vir::LocalDebug,
    /// The local's VIR type (a scalar's encoding).
    pub ty: vir::Ty,
    /// The slot's offset from the frame pointer.
    pub fp_offset: i64,
}

impl FunctionLines {
    /// The lines of `function`, compiled in `ctx` from a translation whose source location
    /// table is `srclocs`; `None` without compiled code.
    pub(crate) fn new(
        id: FuncId,
        function: &vir::Function,
        ctx: &Context,
        srclocs: &[SrcLoc],
    ) -> Option<FunctionLines> {
        let code = ctx.compiled_code()?;
        let size = code.code_info().total_size;
        let decl = function.first_loc();
        let mut rows: Vec<(u32, Option<SrcLoc>)> = vec![(0, decl)];
        let mut body = None;
        let mut end = 0;
        for range in code.buffer.get_srclocs_sorted() {
            if range.loc.is_default() || range.start >= range.end {
                continue;
            }
            let loc = srclocs.get(range.loc.bits() as usize).copied();
            if range.start > end && end > 0 {
                push_row(&mut rows, end, None);
            }
            if body.is_none() {
                // The prologue keeps its own row even on the same line: debuggers put a
                // function's breakpoint at the start of its second row.
                body = Some(range.start);
                if range.start > 0 {
                    rows.push((range.start, loc));
                } else {
                    rows[0].1 = loc;
                }
            } else {
                push_row(&mut rows, range.start, loc);
            }
            end = range.end;
        }
        if end > 0 && end < size {
            push_row(&mut rows, end, None);
        }
        let mut vars: Vec<(u64, Variable)> = vec![];
        if let Some(frame) = code.buffer.frame_layout() {
            for slot in frame.stackslots.values() {
                let Some(key) = slot.key.map(|k| k.bits()) else {
                    continue;
                };
                let Some(local) = function.locals.get(key as usize) else {
                    continue;
                };
                if let (Some(debug), Some(name)) = (&local.debug, &local.name) {
                    let fp_offset = i64::from(slot.offset) - i64::from(frame.frame_to_fp_offset);
                    let var = Variable {
                        name: name.clone(),
                        debug: debug.clone(),
                        ty: local.ty,
                        fp_offset,
                    };
                    vars.push((key, var));
                }
            }
        }
        vars.sort_by_key(|(k, _)| *k);
        Some(FunctionLines {
            id,
            symbol: function.symbol.clone(),
            name: velt_vir::mangle::demangle(&function.symbol),
            external: function.linkage == vir::Linkage::Export,
            size,
            decl,
            rows,
            body: body.unwrap_or(0),
            vars: vars.into_iter().map(|(_, v)| v).collect(),
        })
    }
}

/// Append a row, merging it with the previous one when it changes nothing.
fn push_row(rows: &mut Vec<(u32, Option<SrcLoc>)>, offset: u32, loc: Option<SrcLoc>) {
    match rows.last_mut() {
        Some(last) if last.0 == offset => last.1 = loc,
        Some(last) if last.1 == loc => {}
        _ => rows.push((offset, loc)),
    }
}

/// DWARF 4, 64-bit addresses: what the LLVM backend emits outside Windows.
pub(crate) const ENCODING: Encoding = Encoding {
    format: Format::Dwarf32,
    version: 4,
    address_size: 8,
};

/// Whether code for `triple` gets DWARF variables: its objects carry DWARF (ELF, Mach-O and
/// COFF, `object::add_debug_info`) and its frame pointer has a DWARF number
/// ([`frame_register`]). Only then do described locals need shadow slots.
pub(crate) fn describes_variables(triple: &target_lexicon::Triple) -> bool {
    use target_lexicon::{Architecture as A, BinaryFormat as F};
    matches!(triple.binary_format, F::Elf | F::Macho | F::Coff)
        && matches!(triple.architecture, A::X86_64 | A::Aarch64(_))
}

/// The DWARF number of the frame pointer register, which variables are located from; `None`
/// for other architectures (no variables).
pub(crate) fn frame_register(arch: Architecture) -> Option<gimli::Register> {
    match arch {
        Architecture::X86_64 => Some(gimli::X86_64::RBP),
        Architecture::Aarch64 => Some(gimli::AArch64::X29),
        _ => None,
    }
}

/// One compile unit describing `functions` of `program`; `address(i)` is the start of
/// `functions[i]` (a symbol for objects, a constant for JIT code), and `frame` the frame
/// pointer register (variables are described only with one).
pub(crate) fn build_unit(
    program: &vir::Program,
    functions: &[FunctionLines],
    address: impl Fn(usize) -> Address,
    frame: Option<gimli::Register>,
) -> DwarfUnit {
    let files = &program.files;
    let mut types = types::Types::new(program);
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string().replace('\\', "/"))
        .unwrap_or_default();
    let line_string = |s: &str| LineString::String(dwarf_str(s).into_bytes());
    let (main_name, _) = file_and_directory(files.first().map_or("<velt>", |f| f), &cwd);
    let mut program = LineProgram::new(
        ENCODING,
        LineEncoding::default(),
        line_string(&cwd),
        None,
        line_string(&main_name),
        None,
    );
    let file_ids: Vec<_> = files
        .iter()
        .map(|path| {
            let (name, dir) = file_and_directory(path, &cwd);
            let dir = program.add_directory(line_string(&dir));
            program.add_file(line_string(&name), dir, None)
        })
        .collect();
    let file_of = |loc: SrcLoc| file_ids.get(loc.file as usize).copied();

    for (i, f) in functions.iter().enumerate() {
        program.begin_sequence(Some(address(i)));
        for &(offset, loc) in &f.rows {
            let row = program.row();
            row.address_offset = u64::from(offset);
            row.prologue_end = offset == f.body && offset > 0;
            match loc.and_then(|l| Some((file_of(l)?, l))) {
                Some((file, l)) => {
                    row.file = file;
                    row.line = u64::from(l.line);
                    row.column = u64::from(l.col);
                }
                None => {
                    row.line = 0;
                    row.column = 0;
                }
            }
            program.generate_row();
        }
        program.end_sequence(u64::from(f.size));
    }

    let mut dwarf = DwarfUnit::new(ENCODING);
    dwarf.unit.line_program = program;
    let ranges = RangeList(
        functions
            .iter()
            .enumerate()
            .map(|(i, f)| Range::StartLength {
                begin: address(i),
                length: u64::from(f.size),
            })
            .collect(),
    );
    let ranges = dwarf.unit.ranges.add(ranges);
    let producer = dwarf.strings.add("velt (cranelift)");
    let name = dwarf
        .strings
        .add(dwarf_str(files.first().map_or("<velt>", |f| f.as_str())));
    let comp_dir = dwarf.strings.add(dwarf_str(&cwd));
    let root = dwarf.unit.root();
    let cu = dwarf.unit.get_mut(root);
    cu.set(gimli::DW_AT_producer, AttributeValue::StringRef(producer));
    cu.set(
        gimli::DW_AT_language,
        AttributeValue::Language(gimli::DW_LANG_C),
    );
    cu.set(gimli::DW_AT_name, AttributeValue::StringRef(name));
    cu.set(gimli::DW_AT_comp_dir, AttributeValue::StringRef(comp_dir));
    cu.set(
        gimli::DW_AT_low_pc,
        AttributeValue::Address(Address::Constant(0)),
    );
    cu.set(gimli::DW_AT_ranges, AttributeValue::RangeListRef(ranges));

    for (i, f) in functions.iter().enumerate() {
        let name = dwarf.strings.add(dwarf_str(&f.name));
        let linkage_name = dwarf.strings.add(dwarf_str(&f.symbol));
        let sub_id = dwarf.unit.add(root, gimli::DW_TAG_subprogram);
        let sub = dwarf.unit.get_mut(sub_id);
        sub.set(gimli::DW_AT_name, AttributeValue::StringRef(name));
        sub.set(
            gimli::DW_AT_linkage_name,
            AttributeValue::StringRef(linkage_name),
        );
        sub.set(gimli::DW_AT_low_pc, AttributeValue::Address(address(i)));
        sub.set(
            gimli::DW_AT_high_pc,
            AttributeValue::Udata(u64::from(f.size)),
        );
        if f.external {
            sub.set(gimli::DW_AT_external, AttributeValue::Flag(true));
        }
        if let Some(decl) = f.decl {
            if let Some(file) = file_of(decl) {
                sub.set(
                    gimli::DW_AT_decl_file,
                    AttributeValue::FileIndex(Some(file)),
                );
                sub.set(
                    gimli::DW_AT_decl_line,
                    AttributeValue::Udata(u64::from(decl.line)),
                );
            }
        }
        if let (Some(frame), false) = (frame, f.vars.is_empty()) {
            let mut base = Expression::new();
            base.op_reg(frame);
            sub.set(gimli::DW_AT_frame_base, AttributeValue::Exprloc(base));
            // A variable is in scope from its declaration to the end of its block: source
            // ranges that nest (blocks do), so each variable's lexical block goes inside the one
            // of the innermost variable whose range holds its own. Debuggers expect sibling
            // blocks not to overlap.
            let mut vars: Vec<&Variable> = f.vars.iter().collect();
            vars.sort_by_key(|v| {
                (
                    !v.debug.param,
                    v.debug.decl.line,
                    Reverse(end_key(&v.debug)),
                )
            });
            let mut open: Vec<(&vir::LocalDebug, UnitEntryId)> = vec![];
            let mut budget = scope_range_budget(f);
            for v in vars {
                while open.last().is_some_and(|(o, _)| !holds(o, &v.debug)) {
                    open.pop();
                }
                let parent = open.last().map_or(sub_id, |(_, e)| *e);
                let ranges = scope_ranges(f, &v.debug).filter(|r| r.len() <= budget);
                let parent = match ranges {
                    None => parent,
                    Some(ranges) => {
                        budget -= ranges.len();
                        let ranges = ranges
                            .into_iter()
                            .map(|(begin, end)| Range::StartLength {
                                begin: offset(address(i), begin),
                                length: u64::from(end - begin),
                            })
                            .collect();
                        let ranges = dwarf.unit.ranges.add(RangeList(ranges));
                        let block = dwarf.unit.add(parent, gimli::DW_TAG_lexical_block);
                        dwarf
                            .unit
                            .get_mut(block)
                            .set(gimli::DW_AT_ranges, AttributeValue::RangeListRef(ranges));
                        block
                    }
                };
                if !v.debug.param {
                    open.push((&v.debug, parent));
                }
                add_variable(&mut dwarf, &mut types, parent, v, file_of(v.debug.decl));
            }
        }
    }
    dwarf
}

/// The code ranges (offsets in `f`) where `var` is in scope: code located from its declaration
/// line (a whole line in one block: debuggers make a breakpoint location per block a line's
/// code is in) up to the end of its block, with the code without a location that follows such
/// code. `None` when that is the whole function (params, variables of the function's own block
/// declared at its start).
fn scope_ranges(f: &FunctionLines, var: &vir::LocalDebug) -> Option<Vec<(u32, u32)>> {
    if var.param {
        return None;
    }
    let inside = |l: &Option<SrcLoc>| {
        l.is_some_and(|l| {
            l.file == var.decl.file
                && l.line >= var.decl.line
                && var.end.is_none_or(|e| at(l) < at(e))
        })
    };
    let mut ranges: Vec<(u32, u32)> = vec![];
    let mut whole = true;
    // Whether the code before the row is in scope.
    let mut before = false;
    for (k, (start, loc)) in f.rows.iter().enumerate() {
        let end = f.rows.get(k + 1).map_or(f.size, |r| r.0);
        if *start >= end {
            continue;
        }
        // Code without a location (the few bytes between two located instructions) goes with
        // the code before it. Left out, it would cut the scope of every variable declared
        // above it in two: a function of n declarations in a row would need n² ranges.
        before = match loc {
            Some(_) => inside(loc),
            None => before,
        };
        if !before {
            whole = false;
            continue;
        }
        match ranges.last_mut() {
            Some(last) if last.1 == *start => last.1 = end,
            _ => ranges.push((*start, end)),
        }
    }
    (!whole).then_some(ranges)
}

/// How many scope ranges the variables of `f` may have together: in proportion to its code, so
/// the debug info of a function stays linear in its size. Scopes are a few ranges each unless
/// the code is laid out far from source order; a variable whose ranges no longer fit is shown
/// in the scope around it instead (from before its declaration, as params are).
fn scope_range_budget(f: &FunctionLines) -> usize {
    64 + 4 * f.rows.len()
}

/// A source position, ordered.
fn at(l: SrcLoc) -> (u32, u32) {
    (l.line, l.col)
}

/// The end of a variable's scope, ordered (`None`, the function's end, is last).
fn end_key(v: &vir::LocalDebug) -> (bool, (u32, u32)) {
    (v.end.is_none(), v.end.map_or((0, 0), at))
}

/// Whether `outer`'s scope holds `inner`'s (both in one file).
fn holds(outer: &vir::LocalDebug, inner: &vir::LocalDebug) -> bool {
    outer.decl.file == inner.decl.file
        && outer.decl.line <= inner.decl.line
        && end_key(inner) <= end_key(outer)
}

/// `a` plus `by` bytes.
fn offset(a: Address, by: u32) -> Address {
    match a {
        Address::Constant(c) => Address::Constant(c + u64::from(by)),
        Address::Symbol { symbol, addend } => Address::Symbol {
            symbol,
            addend: addend + i64::from(by),
        },
    }
}

/// A `DW_TAG_variable` (or `DW_TAG_formal_parameter`) for `v` under `parent` (its subprogram or
/// lexical block).
fn add_variable(
    dwarf: &mut DwarfUnit,
    types: &mut types::Types<'_>,
    parent: UnitEntryId,
    v: &Variable,
    file: Option<FileId>,
) {
    // A local holding the variable by reference holds a pointer: the value is the pointee, in
    // its natural representation.
    let held = (!v.debug.by_ref).then_some(v.ty);
    let ty = types.ty(dwarf, v.debug.ty, held);
    let tag = match v.debug.param {
        true => gimli::DW_TAG_formal_parameter,
        false => gimli::DW_TAG_variable,
    };
    let name = dwarf.strings.add(dwarf_str(&v.name));
    let mut location = Expression::new();
    location.op_fbreg(v.fp_offset);
    if v.debug.by_ref {
        location.op_deref();
    }
    let var = dwarf.unit.add(parent, tag);
    let var = dwarf.unit.get_mut(var);
    var.set(gimli::DW_AT_name, AttributeValue::StringRef(name));
    var.set(gimli::DW_AT_type, AttributeValue::UnitRef(ty));
    var.set(gimli::DW_AT_location, AttributeValue::Exprloc(location));
    if let Some(file) = file {
        var.set(
            gimli::DW_AT_decl_file,
            AttributeValue::FileIndex(Some(file)),
        );
        var.set(
            gimli::DW_AT_decl_line,
            AttributeValue::Udata(u64::from(v.debug.decl.line)),
        );
    }
}

/// `(file name, directory)` of a source path: relative paths are relative to the current
/// directory (where the compiler ran), absolute ones stand alone (as in the LLVM backend).
fn file_and_directory(path: &str, cwd: &str) -> (String, String) {
    let path = path.replace('\\', "/");
    let absolute = path.starts_with('/') || path.get(1..3).is_some_and(|s| s == ":/");
    match path.rsplit_once('/') {
        Some((dir, name)) if absolute => (name.to_string(), dir.to_string()),
        _ if absolute => (path, String::new()),
        _ => (path, cwd.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::file_and_directory;

    #[test]
    fn paths() {
        assert_eq!(
            file_and_directory("/usr/lib/a.vlt", "/w"),
            ("a.vlt".into(), "/usr/lib".into())
        );
        assert_eq!(
            file_and_directory("C:\\x\\b.vlt", "C:/w"),
            ("b.vlt".into(), "C:/x".into())
        );
        assert_eq!(
            file_and_directory("examples/foo.vlt", "/w"),
            ("examples/foo.vlt".into(), "/w".into())
        );
    }
}

/// `s` as a DWARF string, which is NUL-terminated: a NUL inside it (a quoted field name such as
/// `"a\u0000b"` that ends up in a function's name) is written `\0`.
fn dwarf_str(s: &str) -> String {
    s.replace('\0', "\\0")
}

#[cfg(test)]
mod dwarf_str_tests {
    #[test]
    fn a_nul_is_escaped() {
        assert_eq!(super::dwarf_str("a\0b"), "a\\0b");
        assert_eq!(super::dwarf_str("plain"), "plain");
    }
}
