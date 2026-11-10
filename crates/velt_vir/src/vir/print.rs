//! The text form of a whole program (`Display for Program`), for dumps and tests.

use std::fmt;

use super::{fmt_tys, Callee, Linkage, Program, Terminator};

impl fmt::Display for Program {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, a) in self.aggs.iter().enumerate() {
            let fields: Vec<String> = a.fields.iter().map(|(t, o)| format!("{t}@{o}")).collect();
            writeln!(
                f,
                "agg#{} {} size={} align={} {{ {} }}",
                i,
                a.name,
                a.size,
                a.align,
                fields.join(", ")
            )?;
        }
        for (i, s) in self.statics.iter().enumerate() {
            let mutable = s.mutable.map(|id| format!(" mutable={id:x}"));
            // A named static (invariant 13); unnamed ones print as before.
            let named = (s.symbol.as_ref()).map(|sym| {
                let how = match s.linkage {
                    Linkage::Import => "import",
                    _ => "export",
                };
                format!(" symbol={sym} {how}")
            });
            write!(f, "static#{} align={} ", i, s.align)?;
            write_static_bytes(f, &s.bytes)?;
            writeln!(
                f,
                "{}{}",
                mutable.unwrap_or_default(),
                named.unwrap_or_default()
            )?;
        }
        for (i, e) in self.externs.iter().enumerate() {
            let nr = if e.noreturn { " noreturn" } else { "" };
            let (sym, mut params) = (&e.symbol, fmt_tys(&e.params));
            if e.variadic {
                params.push_str(if e.params.is_empty() { "..." } else { ", ..." });
            }
            let host = e
                .host
                .as_ref()
                .map(|h| format!(" import {:?}", h.manifest_line()));
            let host = host.unwrap_or_default();
            writeln!(f, "extern#{i} {sym}({params}) -> {}{nr}{host}", e.ret)?;
        }
        for (i, func) in self.funcs.iter().enumerate() {
            let link = match func.linkage {
                Linkage::Export => "export",
                Linkage::Internal => "internal",
                Linkage::Import => "import",
            };
            writeln!(f)?;
            let open = if func.blocks.is_empty() { "" } else { " {" };
            writeln!(
                f,
                "fn#{} {} {}({}) -> {}{}{}{open}",
                i,
                link,
                func.symbol,
                fmt_tys(&func.params),
                func.ret,
                if func.cold { " cold" } else { "" },
                match func.inline {
                    crate::vir::Inline::Default => "",
                    crate::vir::Inline::Always => " alwaysinline",
                    crate::vir::Inline::Never => " noinline",
                }
            )?;
            if open.is_empty() {
                continue;
            }
            for (li, l) in func.locals.iter().enumerate() {
                let kind = if li < func.params.len() {
                    "param"
                } else {
                    "let"
                };
                let attrs = func.param_attr(li);
                let attrs = if li < func.params.len() && !attrs.is_empty() {
                    format!(" {attrs}")
                } else {
                    String::new()
                };
                match &l.name {
                    Some(n) => writeln!(f, "  {kind} _{li}: {}{attrs} // {n}", l.ty)?,
                    None => writeln!(f, "  {kind} _{li}: {}{attrs}", l.ty)?,
                }
            }
            for (bi, b) in func.blocks.iter().enumerate() {
                writeln!(f, "  bb{bi}:")?;
                for s in &b.stmts {
                    writeln!(f, "    {s}")?;
                }
                writeln!(f, "    {}", self.fmt_term(&b.term))?;
            }
            writeln!(f, "}}")?;
        }
        Ok(())
    }
}

/// From this size, a static that isn't text prints as hex (`hex"00ff"`). Escaping a table of numbers
/// as a string costs hundreds of instructions per byte and loses what isn't UTF-8 (#2501).
const HEX_FROM: usize = 1024;

/// A static's bytes: a quoted string, or hex for a large one that isn't text.
fn write_static_bytes(f: &mut fmt::Formatter<'_>, bytes: &[u8]) -> fmt::Result {
    if bytes.len() < HEX_FROM || std::str::from_utf8(bytes).is_ok() {
        return write!(f, "{:?}", String::from_utf8_lossy(bytes));
    }
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut hex = Vec::with_capacity(bytes.len() * 2);
    for &b in bytes {
        hex.push(DIGITS[(b >> 4) as usize]);
        hex.push(DIGITS[(b & 15) as usize]);
    }
    let hex = String::from_utf8(hex).expect("ICE: hex digits are ASCII");
    write!(f, "hex\"{hex}\"")
}

impl Program {
    fn fmt_callee(&self, c: &Callee) -> String {
        match c {
            Callee::Func(id) => match self.funcs.get(id.0 as usize) {
                Some(func) => format!("fn#{} {}", id.0, func.symbol),
                None => format!("fn#{}", id.0),
            },
            Callee::Extern(id) => match self.externs.get(id.0 as usize) {
                Some(e) => format!("extern#{} {}", id.0, e.symbol),
                None => format!("extern#{}", id.0),
            },
            Callee::Ptr {
                target,
                params,
                ret,
            } => format!("({target}: fn({}) -> {ret})", fmt_tys(params)),
        }
    }

    fn fmt_term(&self, t: &Terminator) -> String {
        match t {
            Terminator::Goto(b) => format!("goto bb{}", b.0),
            Terminator::Branch { cond, then, els } => {
                format!("branch {cond}, bb{}, bb{}", then.0, els.0)
            }
            Terminator::Switch {
                value,
                cases,
                default,
            } => {
                let cs: Vec<String> = cases
                    .iter()
                    .map(|(v, b)| format!("{v} => bb{}", b.0))
                    .collect();
                format!(
                    "switch {value} [{}], default bb{}",
                    cs.join(", "),
                    default.0
                )
            }
            Terminator::Return(o) => format!("return {o}"),
            Terminator::Call {
                callee,
                args,
                dest,
                next,
                unwind,
            } => {
                let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
                let call = format!("call {}({})", self.fmt_callee(callee), args.join(", "));
                let pad = match unwind {
                    Some(p) => format!(" unwind bb{}", p.0),
                    None => String::new(),
                };
                match dest {
                    Some(d) => format!("{d} = {call} -> bb{}{pad}", next.0),
                    None => format!("{call} -> bb{}{pad}", next.0),
                }
            }
            Terminator::Unreachable => "unreachable".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Program, StaticData};
    use super::HEX_FROM;

    fn line(bytes: Vec<u8>) -> String {
        let data = StaticData {
            bytes,
            align: 1,
            ..Default::default()
        };
        let p = Program {
            statics: vec![data],
            ..Default::default()
        };
        p.to_string().trim_end().to_string()
    }

    #[test]
    fn a_large_static_that_is_not_text_prints_as_hex() {
        let mut bytes = vec![0x41; HEX_FROM];
        bytes[0] = 0xff;
        let want = format!("static#0 align=1 hex\"ff{}\"", "41".repeat(HEX_FROM - 1));
        assert_eq!(line(bytes), want);
    }

    #[test]
    fn text_and_small_tables_print_as_strings() {
        assert_eq!(line(b"hi\n".to_vec()), "static#0 align=1 \"hi\\n\"");
        assert_eq!(
            line(vec![0xff; HEX_FROM - 1]).matches('\u{fffd}').count(),
            HEX_FROM - 1
        );
        let text = vec![b'a'; HEX_FROM * 2];
        assert!(line(text).contains("\"aaa"));
    }
}
