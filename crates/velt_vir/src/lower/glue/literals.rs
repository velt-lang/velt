//! Text of values that carry no payload bits: literal types (`TyKind::Literal`, zero-sized —
//! the type is the value) and string enum members (an `I64` discriminant whose text is the
//! member's `VariantDef::str_value`). Used by the formatter (`console.log`, `${x}`) and by
//! `JSON.stringify`.

use velt_sema::hir::{LitValue, TyId, TyKind};

use crate::lower::rt::Rt;
use crate::lower::FnLower;
use crate::vir::{self, Const, Operand, Place, Terminator, Ty, STR_AGG};

impl FnLower<'_, '_> {
    /// The literal type of union variant `v` of `ty`, if that member is a literal type.
    pub(in crate::lower) fn variant_literal(&mut self, ty: TyId, v: u32) -> Option<LitValue> {
        let parts = self.cx.variant_tys(ty, v);
        match parts.as_slice() {
            [t] => match self.cx.kind(*t) {
                TyKind::Literal(l) => Some(l),
                _ => None,
            },
            _ => None,
        }
    }

    /// Append the console text of a literal value (strings quoted when `nested`, like node).
    pub(in crate::lower) fn push_literal(&mut self, buf: &Operand, l: &LitValue, nested: bool) {
        match l {
            LitValue::Str(s) if nested => self.push_text(buf, &inspect_quote(s)),
            LitValue::Str(s) => self.push_text(buf, s),
            LitValue::Bool(b) => self.push_text(buf, if *b { "true" } else { "false" }),
            LitValue::Int(_, n) => self.push_text(buf, &n.to_string()),
            LitValue::Float(_, bits) => {
                let f = Operand::Const(Const::Float(f64::from_bits(*bits)), Ty::F64);
                self.call_rt(Rt::StrbufPushF64, vec![buf.clone(), f], None);
            }
        }
    }

    /// Append the JSON text of a literal value.
    pub(in crate::lower) fn push_literal_json(&mut self, buf: &Operand, l: &LitValue) {
        match l {
            LitValue::Str(s) => {
                let v = self.str_lit(s);
                let a = self.operand_addr(v, Ty::Agg(STR_AGG));
                self.call_rt(Rt::StrbufPushJsonStr, vec![buf.clone(), a], None);
            }
            LitValue::Bool(b) => self.push_text(buf, if *b { "true" } else { "false" }),
            LitValue::Int(_, n) => self.push_text(buf, &n.to_string()),
            LitValue::Float(_, bits) => {
                let f = Operand::Const(Const::Float(f64::from_bits(*bits)), Ty::F64);
                self.call_rt(Rt::StrbufPushJsonF64, vec![buf.clone(), f], None);
            }
        }
    }

    /// The member strings of string enum type `ty` (`None` for other types).
    pub(in crate::lower) fn enum_strings(&self, ty: TyId) -> Option<Vec<String>> {
        let TyKind::Adt(d, _) = self.cx.kind(ty) else {
            return None;
        };
        let velt_sema::hir::Def::Enum(e) = self.cx.hir.def(d) else {
            return None;
        };
        e.variants
            .iter()
            .map(|v| v.str_value.clone())
            .collect::<Option<Vec<String>>>()
            .filter(|vs| !vs.is_empty())
    }

    /// Append the string of the string-enum value at `place` (its discriminant is the member
    /// index): quoted when `nested`, as a JSON string when `json`.
    pub(in crate::lower) fn push_enum_str(
        &mut self,
        buf: &Operand,
        place: &Place,
        strings: &[String],
        nested: bool,
        json: bool,
    ) {
        let join = self.new_block();
        let blocks: Vec<vir::BlockId> = strings.iter().map(|_| self.new_block()).collect();
        let cases = blocks
            .iter()
            .enumerate()
            .map(|(i, b)| (i as i128, *b))
            .collect();
        self.terminate(Terminator::Switch {
            value: Operand::Copy(place.clone()),
            cases,
            default: join,
        });
        for (s, b) in strings.iter().zip(blocks) {
            self.switch_to(b);
            let l = LitValue::Str(s.clone());
            if json {
                self.push_literal_json(buf, &l);
            } else {
                self.push_literal(buf, &l, nested);
            }
            self.goto(join);
        }
        self.switch_to(join);
    }
}

/// `s` quoted and escaped the way `console.log` shows a string inside a container (node's
/// `util.inspect`; the runtime's `velt_rt_strbuf_push_inspect_str` does the same at run time).
pub(crate) fn inspect_quote(s: &str) -> String {
    let quote = if !s.contains('\'') {
        '\''
    } else if !s.contains('"') {
        '"'
    } else if !s.contains('`') && !s.contains("${") {
        '`'
    } else {
        '\''
    };
    let mut out = String::from(quote);
    for c in s.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            '\\' => out.push_str("\\\\"),
            _ if c == quote => {
                out.push('\\');
                out.push(c);
            }
            '\0'..='\u{1f}' | '\u{7f}'..='\u{9f}' => out.push_str(&format!("\\x{:02X}", c as u32)),
            _ => out.push(c),
        }
    }
    out.push(quote);
    out
}
