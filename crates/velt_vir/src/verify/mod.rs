//! VIR verifier: checks the invariants listed in the `vir.rs` header — program-level layout and
//! symbol rules here, per-function typing in typing.rs, definite assignment in init.rs.

mod debug;
mod dominators;
mod init;
mod typing;

use std::collections::HashSet;

use crate::vir::*;

pub(crate) fn verify_program(p: &Program) -> Result<(), Vec<String>> {
    let mut errs = vec![];
    check_aggs(p, &mut errs);
    for (i, e) in p.externs.iter().enumerate() {
        if e.params.iter().any(|t| !t.is_scalar()) || matches!(e.ret, Ty::Agg(_)) {
            errs.push(format!(
                "extern#{i} {}: signature is not scalar-only",
                e.symbol
            ));
        }
    }
    check_symbols(p, &mut errs);
    check_statics(p, &mut errs);
    debug::check_debug(p, &mut errs);
    for (i, f) in p.funcs.iter().enumerate() {
        check_locs(p, f, &mut errs);
        check_param_attrs(f, &mut errs);
        FnCheck {
            p,
            f,
            errs: &mut errs,
            ctx: format!("fn#{i} {}", f.symbol),
        }
        .run();
    }
    if errs.is_empty() {
        Ok(())
    } else {
        Err(errs)
    }
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
    let mut syms = HashSet::new();
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
        Some(_) => errs.push("velt_main must be `export velt_main() -> I32`".into()),
        None => errs.push("missing exported entry `velt_main`".into()),
    }
}

/// Alignment and relocation rules of `StaticData` (see its docs in vir.rs).
fn check_statics(p: &Program, errs: &mut Vec<String>) {
    for (i, s) in p.statics.iter().enumerate() {
        if !s.align.is_power_of_two() {
            errs.push(format!("static#{i}: bad alignment {}", s.align));
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
            self.err(None, "has no blocks".into());
            return false;
        }
        true
    }
}
