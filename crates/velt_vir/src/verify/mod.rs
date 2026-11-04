//! VIR verifier: checks the invariants listed in the `vir.rs` header — program-level layout and
//! symbol rules here, per-function typing in typing.rs, definite assignment in init.rs.

mod debug;
mod dominators;
mod init;
mod typing;

#[cfg(test)]
mod parallel_tests;

use rustc_hash::FxHashSet;

use crate::vir::*;

/// A `velt_simd_*` extern has no runtime function: backends emit it inline from what its
/// symbol names, so it must name an operation and have that operation's signature.
fn check_simd_extern(i: usize, e: &ExternFn, errs: &mut Vec<String>) {
    if !e.symbol.starts_with("velt_simd_") {
        return;
    }
    match crate::simd::SimdCall::of_symbol(&e.symbol) {
        None => errs.push(format!("extern#{i} {}: not a SIMD operation", e.symbol)),
        Some(call) if call.signature() != (e.params.clone(), e.ret) => errs.push(format!(
            "extern#{i} {}: signature differs from the SIMD operation's",
            e.symbol
        )),
        Some(_) => {}
    }
}

/// Whether a symbol names an operation backends emit inline (SIMD, atomics): no function
/// exists behind it, so it is only ever called directly.
pub(crate) fn inline_extern(symbol: &str) -> bool {
    symbol.starts_with("velt_atomic_") || symbol.starts_with("velt_simd_")
}

/// A `velt_atomic_*` extern likewise (`crate::atomic`): an operation backends emit inline.
fn check_atomic_extern(i: usize, e: &ExternFn, errs: &mut Vec<String>) {
    if !e.symbol.starts_with("velt_atomic_") {
        return;
    }
    if e.noreturn {
        errs.push(format!(
            "extern#{i} {}: an atomic operation returns",
            e.symbol
        ));
    }
    match crate::atomic::AtomicCall::of_symbol(&e.symbol) {
        None => errs.push(format!("extern#{i} {}: not an atomic operation", e.symbol)),
        Some(call) if call.signature() != (e.params.clone(), e.ret) => errs.push(format!(
            "extern#{i} {}: signature differs from the atomic operation's",
            e.symbol
        )),
        Some(_) => {}
    }
}

/// A runtime math function (`crate::math`) likewise: backends and passes treat a call of it as
/// its operation, so the extern must have the operation's signature.
fn check_math_extern(i: usize, e: &ExternFn, errs: &mut Vec<String>) {
    let Some(op) = crate::math::MathOp::of_symbol(&e.symbol) else {
        return;
    };
    if op.signature() != (e.params.as_slice(), e.ret) || e.noreturn {
        errs.push(format!(
            "extern#{i} {}: signature differs from the math operation's",
            e.symbol
        ));
    }
}

pub(crate) fn verify_program(p: &Program) -> Result<(), Vec<String>> {
    verify_program_on(p, crate::parallel::jobs())
}

/// [`verify_program`] on up to `jobs` threads.
fn verify_program_on(p: &Program, jobs: usize) -> Result<(), Vec<String>> {
    let mut errs = vec![];
    check_aggs(p, &mut errs);
    for (i, e) in p.externs.iter().enumerate() {
        if e.params.iter().any(|t| !t.is_scalar()) || matches!(e.ret, Ty::Agg(_)) {
            errs.push(format!(
                "extern#{i} {}: signature is not scalar-only",
                e.symbol
            ));
        }
        check_simd_extern(i, e, &mut errs);
        check_atomic_extern(i, e, &mut errs);
        check_math_extern(i, e, &mut errs);
    }
    check_symbols(p, &mut errs);
    check_statics(p, &mut errs);
    debug::check_debug(p, &mut errs);
    // Each function on its own, on every core for larger programs: its errors are listed
    // after the program's, in the functions' order, whatever the number of threads.
    let per_chunk = crate::parallel::map_chunks(
        &p.funcs,
        FUNCTIONS_PER_CHUNK,
        PARALLEL_MIN_FUNCTIONS,
        jobs,
        |first, funcs| {
            let mut errs = vec![];
            for (i, f) in (first..).zip(funcs) {
                check_function(p, i, f, &mut errs);
            }
            errs
        },
    );
    errs.extend(per_chunk.into_iter().flatten());
    if errs.is_empty() {
        Ok(())
    } else {
        Err(errs)
    }
}

/// Below this many functions a program is checked on the calling thread: starting threads
/// costs more than it saves.
const PARALLEL_MIN_FUNCTIONS: usize = 256;

/// Functions a worker checks at a time.
const FUNCTIONS_PER_CHUNK: usize = 32;

/// The checks of function `i`.
fn check_function(p: &Program, i: usize, f: &Function, errs: &mut Vec<String>) {
    check_locs(p, f, errs);
    check_param_attrs(f, errs);
    FnCheck {
        p,
        f,
        errs,
        ctx: format!("fn#{i} {}", f.symbol),
    }
    .run();
}

fn check_aggs(p: &Program, errs: &mut Vec<String>) {
    let str_fields = vec![(Ty::U64, 0), (Ty::U64, 8), (Ty::U64, 16)];
    if !p
        .aggs
        .first()
        .is_some_and(|a| a.size == 24 && a.align == 8 && a.fields == str_fields)
    {
        errs.push(
            "aggs[0] must be the VeltStr layout {U64@0, U64@8, U64@16} size 24 align 8".into(),
        );
    }
    for (i, a) in p.aggs.iter().enumerate() {
        if !a.align.is_power_of_two() {
            errs.push(format!("agg#{i} {}: bad alignment {}", a.name, a.align));
        }
        for (fi, (fty, off)) in a.fields.iter().enumerate() {
            // Fields may only reference earlier aggregates, which also rules out cycles.
            let bad = match fty {
                Ty::Agg(id) => id.0 as usize >= i,
                t => *t == Ty::Unit,
            };
            if bad {
                errs.push(format!(
                    "agg#{i} {}: field {fi} has an invalid type {fty:?}",
                    a.name
                ));
            } else if off + p.size_align(*fty).0 > a.size {
                errs.push(format!(
                    "agg#{i} {}: field {fi} exceeds aggregate size",
                    a.name
                ));
            }
        }
    }
}

/// Invariant 8: `locs` is empty or matches the block/statement shape, and names known files.
fn check_locs(p: &Program, f: &Function, errs: &mut Vec<String>) {
    if f.locs.is_empty() {
        return;
    }
    if f.locs.len() != f.blocks.len() {
        errs.push(format!(
            "{}: locs has {} blocks but the function has {}",
            f.symbol,
            f.locs.len(),
            f.blocks.len()
        ));
        return;
    }
    for (bi, (b, locs)) in f.blocks.iter().zip(&f.locs).enumerate() {
        if locs.len() != b.stmts.len() + 1 {
            errs.push(format!(
                "{}: bb{bi} has {} statements but {} locs (want statements + 1)",
                f.symbol,
                b.stmts.len(),
                locs.len()
            ));
        }
        if let Some(l) = locs
            .iter()
            .flatten()
            .find(|l| l.file as usize >= p.files.len())
        {
            errs.push(format!(
                "{}: bb{bi}: loc names unknown file {}",
                f.symbol, l.file
            ));
        }
    }
}

/// Invariant 9: `param_attrs` is empty or has one entry per param; only `Ptr` params carry
/// attributes.
fn check_param_attrs(f: &Function, errs: &mut Vec<String>) {
    if f.param_attrs.is_empty() {
        return;
    }
    if f.param_attrs.len() != f.params.len() {
        errs.push(format!(
            "{}: param_attrs has {} entries but the function has {} params",
            f.symbol,
            f.param_attrs.len(),
            f.params.len()
        ));
        return;
    }
    for (i, (a, t)) in f.param_attrs.iter().zip(&f.params).enumerate() {
        if !a.is_empty() && *t != Ty::Ptr {
            errs.push(format!(
                "{}: param {i} of type {t} has pointer attributes {a}",
                f.symbol
            ));
        }
    }
}

fn check_symbols(p: &Program, errs: &mut Vec<String>) {
    let mut syms = FxHashSet::default();
    for f in &p.funcs {
        if !syms.insert(f.symbol.as_str()) {
            errs.push(format!("duplicate function symbol `{}`", f.symbol));
        }
    }
    for e in &p.externs {
        if syms.contains(e.symbol.as_str()) {
            errs.push(format!(
                "extern symbol `{}` collides with a function",
                e.symbol
            ));
        }
    }
    match p.funcs.iter().find(|f| f.symbol == "velt_main") {
        Some(m) if m.params.is_empty() && m.ret == Ty::I32 && m.linkage == Linkage::Export => {}
        // Code compiled on its own to link into a program declares the entry (invariant 5).
        Some(m) if m.params.is_empty() && m.ret == Ty::I32 && m.linkage == Linkage::Import => {}
        Some(_) => errs.push("velt_main must be `export velt_main() -> I32` (or `import`, in code compiled to link into a program)".into()),
        None => errs.push("missing exported entry `velt_main`".into()),
    }
}

/// Alignment and relocation rules of `StaticData` (see its docs in vir.rs).
fn check_statics(p: &Program, errs: &mut Vec<String>) {
    // A static's symbol shares the link's namespace with the functions and externs.
    let mut symbols: std::collections::HashSet<&str> = std::collections::HashSet::new();
    if p.statics.iter().any(|s| s.symbol.is_some()) {
        symbols.extend(p.funcs.iter().map(|f| f.symbol.as_str()));
        symbols.extend(p.externs.iter().map(|e| e.symbol.as_str()));
    }
    for (i, s) in p.statics.iter().enumerate() {
        if !s.align.is_power_of_two() {
            errs.push(format!("static#{i}: bad alignment {}", s.align));
        }
        if s.mutable.is_some() && !s.relocs.is_empty() {
            errs.push(format!("static#{i}: writable data with relocations"));
        }
        // One word: `velt dev` keeps each in an 8-byte cell (velt_codegen_cl dev/mod.rs).
        let word = matches!(s.bytes.len(), 1 | 2 | 4 | 8) && s.align as usize == s.bytes.len();
        if s.mutable.is_some() && !word {
            errs.push(format!(
                "static#{i}: writable data that is not one aligned word"
            ));
        }
        let mut offsets: Vec<u32> = s.relocs.iter().map(|(off, _)| *off).collect();
        offsets.sort_unstable();
        if offsets.windows(2).any(|w| w[1] - w[0] < 8) {
            errs.push(format!("static#{i}: overlapping relocations"));
        }
        for (off, target) in &s.relocs {
            if let Err(e) = check_reloc(p, &s.bytes, *off, target) {
                errs.push(format!("static#{i}: relocation at {off}: {e}"));
            }
        }
        check_static_symbol(p, i, s, &mut symbols, errs);
    }
}

/// Invariant 13: a static has a symbol exactly when it is `Export` or `Import`, symbols are
/// unique, and an `Import` static's relocations name only what another object can see.
fn check_static_symbol<'p>(
    p: &'p Program,
    i: usize,
    s: &'p crate::vir::StaticData,
    symbols: &mut std::collections::HashSet<&'p str>,
    errs: &mut Vec<String>,
) {
    match (&s.symbol, s.linkage) {
        (None, Linkage::Internal) => return,
        (Some(_), Linkage::Internal) => errs.push(format!("static#{i}: internal with a symbol")),
        (None, _) => errs.push(format!("static#{i}: {:?} without a symbol", s.linkage)),
        (Some(sym), _) => {
            if !symbols.insert(sym) {
                errs.push(format!("static#{i}: symbol `{sym}` is not unique"));
            }
        }
    }
    if s.linkage != Linkage::Import {
        return;
    }
    for (off, target) in &s.relocs {
        let visible = match target {
            Const::Func(f) => p.funcs.get(f.0 as usize).map(|f| f.linkage),
            Const::Static(t) => p.statics.get(t.0 as usize).map(|t| t.linkage),
            _ => Some(Linkage::Export),
        };
        if visible == Some(Linkage::Internal) {
            errs.push(format!(
                "static#{i}: imported, but its relocation at {off} names an internal definition"
            ));
        }
    }
}

fn check_reloc(p: &Program, bytes: &[u8], off: u32, target: &Const) -> Result<(), String> {
    if !off.is_multiple_of(8) {
        return Err("offset is not 8-aligned".into());
    }
    let Some(slot) = bytes.get(off as usize..off as usize + 8) else {
        return Err("slot is out of bounds".into());
    };
    if slot.iter().any(|b| *b != 0) {
        return Err("slot bytes are not zero".into());
    }
    let (id, len, what) = match target {
        Const::Func(x) => (x.0, p.funcs.len(), "function"),
        Const::Extern(x)
            if p.externs
                .get(x.0 as usize)
                .is_some_and(|e| inline_extern(&e.symbol)) =>
        {
            return Err("target is an inline operation, which has no address".into())
        }
        Const::Extern(x) => (x.0, p.externs.len(), "extern"),
        Const::Static(x) => (x.0, p.statics.len(), "static"),
        c => return Err(format!("target {c:?} is not an address")),
    };
    if id as usize >= len {
        return Err(format!("unknown {what} #{id}"));
    }
    Ok(())
}

/// Checks one function; errors are prefixed with `ctx` (`fn#N symbol`).
struct FnCheck<'a> {
    p: &'a Program,
    f: &'a Function,
    errs: &'a mut Vec<String>,
    ctx: String,
}

impl FnCheck<'_> {
    fn err(&mut self, bb: Option<usize>, msg: String) {
        match bb {
            Some(b) => self.errs.push(format!("{} bb{b}: {msg}", self.ctx)),
            None => self.errs.push(format!("{}: {msg}", self.ctx)),
        }
    }

    fn run(mut self) {
        let before = self.errs.len();
        if !self.check_signature() {
            return;
        }
        let f = self.f;
        for (bi, b) in f.blocks.iter().enumerate() {
            for s in &b.stmts {
                if let Err(e) = self.check_stmt(s) {
                    self.err(Some(bi), e);
                }
            }
            if let Err(e) = self.check_term(&b.term) {
                self.err(Some(bi), e);
            }
        }
        // The dataflow indexes locals and blocks, so it needs well-formed ids.
        if self.errs.len() == before {
            self.check_definite_init();
        }
    }

    /// Signature, locals and block list; returns false if the body can't be checked further.
    fn check_signature(&mut self) -> bool {
        let f = self.f;
        for (i, t) in f.params.iter().enumerate() {
            if !t.is_scalar() {
                self.err(None, format!("param {i} has non-scalar type {t:?}"));
            }
        }
        if matches!(f.ret, Ty::Agg(_)) {
            self.err(None, "returns an aggregate".into());
        }
        if f.locals.len() < f.params.len() {
            self.err(None, "fewer locals than params".into());
            return false;
        }
        for (i, t) in f.params.iter().enumerate() {
            if f.locals[i].ty != *t {
                self.err(
                    None,
                    format!(
                        "local _{i} type {:?} differs from param type {t:?}",
                        f.locals[i].ty
                    ),
                );
            }
        }
        // Unit locals are allowed: they serve as dummy call destinations (vir.rs `Ty::Unit`).
        for (i, l) in f.locals.iter().enumerate() {
            if let Ty::Agg(a) = l.ty {
                if a.0 as usize >= self.p.aggs.len() {
                    self.err(
                        None,
                        format!("local _{i} has unknown aggregate agg#{}", a.0),
                    );
                }
            }
        }
        if f.blocks.is_empty() {
            // A declaration of a function the link adds (invariant 11): nothing more to check.
            if f.linkage != Linkage::Import {
                self.err(None, "has no blocks".into());
            }
            return false;
        }
        true
    }
}
