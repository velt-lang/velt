//! The text form of a whole program (`Display for Program`), for dumps and tests.

use std::fmt;

use super::{fmt_tys, Linkage, Program};

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
            writeln!(
                f,
                "static#{} align={} {:?}{}",
                i,
                s.align,
                String::from_utf8_lossy(&s.bytes),
                mutable.unwrap_or_default()
            )?;
        }
        for (i, e) in self.externs.iter().enumerate() {
            let nr = if e.noreturn { " noreturn" } else { "" };
            let (sym, params) = (&e.symbol, fmt_tys(&e.params));
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
                "fn#{} {} {}({}) -> {}{}{open}",
                i,
                link,
                func.symbol,
                fmt_tys(&func.params),
                func.ret,
                if func.cold { " cold" } else { "" }
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
