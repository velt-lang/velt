//! `velt_rt_str_cmp` against a one-byte string literal (`c >= "0"`, `"a" <= c`) without a call.
//!
//! Tokenizers and parsers compare single characters with literals, and each comparison was a
//! runtime call. When one operand of the call is, in the VIR, the address of a local built once
//! as a static one-byte ASCII literal (`{ &bytes, 1 << 32 | 1, 0 }`, rt_abi.md "Strings"), the call
//! goes to an inline helper instead: the other string's first byte decides (a one-byte string is
//! ASCII, so its byte is its UTF-16 code unit, and the lead byte of a longer UTF-8 sequence is
//! above every ASCII byte, as its code unit is), then the longer string is greater. The choice
//! is made per call site from the VIR, so a comparison of two variable strings (`sort()`) stays
//! exactly the plain call it was.

use velt_vir::vir::{
    Const, Function, Local, Operand, Place, Rvalue, Stmt, Terminator, Ty, STR_AGG,
};

use crate::strings::view;

/// `w1` of a static one-byte ASCII string: one code unit, one byte.
const ONE_BYTE_ASCII_W1: i128 = (1 << 32) | 1;

/// The helper to call instead of `velt_rt_str_cmp(args)` in `function`, with its definition,
/// when one argument is a one-byte literal.
pub(crate) fn literal_compare(
    function: &Function,
    symbol: &str,
    params: &[Ty],
    ret: Ty,
    args: &[Operand],
) -> Option<(&'static str, String)> {
    if symbol != "velt_rt_str_cmp" || params != [Ty::Ptr, Ty::Ptr] || ret != Ty::I32 {
        return None;
    }
    if is_one_byte_literal(function, &args[1]) {
        Some(("@velt.str_cmp_byte", helper(false)))
    } else if is_one_byte_literal(function, &args[0]) {
        Some(("@velt.str_byte_cmp", helper(true)))
    } else {
        None
    }
}

/// `velt.str_cmp_byte(s, lit)`, or with `literal_first` `velt.str_byte_cmp(lit, s)`: the
/// comparison of `s` with the one-byte `lit` (negated when the literal is the left operand).
fn helper(literal_first: bool) -> String {
    let (name, params, result) = if literal_first {
        (
            "str_byte_cmp",
            "ptr %l, ptr %s",
            "  %neg = sub i32 0, %r\n  ret i32 %neg",
        )
    } else {
        ("str_cmp_byte", "ptr %s, ptr %l", "  ret i32 %r")
    };
    format!(
        "define internal i32 @velt.{name}({params}) alwaysinline nounwind {{
{}{}  %empty = icmp eq i64 %s.len, 0
  %x = load i8, ptr %s.data, align 1
  %y = load i8, ptr %l.data, align 1
  %lt = icmp ult i8 %x, %y
  %ne = icmp ne i8 %x, %y
  %ord = select i1 %lt, i32 -1, i32 1
  %more = icmp ugt i64 %s.len, 1
  %tail = zext i1 %more to i32
  %first = select i1 %ne, i32 %ord, i32 %tail
  %r = select i1 %empty, i32 -1, i32 %first
{result}
}}",
        view("s"),
        view("l"),
    )
}

/// Whether `op` is `&z` (the only value of a local defined once) where `z` is written once,
/// as a whole, with the words of a static one-byte ASCII string, and its address is taken only
/// there.
fn is_one_byte_literal(function: &Function, op: &Operand) -> bool {
    let Operand::Copy(Place { local: p, proj }) = op else {
        return false;
    };
    if !proj.is_empty() {
        return false;
    }
    let [Rvalue::AddrOf(Place { local: z, proj })] = whole_writes(function, *p)[..] else {
        return false;
    };
    if !proj.is_empty() || addresses_taken(function, *z) != 1 {
        return false;
    }
    match whole_writes(function, *z)[..] {
        [Rvalue::Aggregate(agg, fields)] => {
            *agg == STR_AGG
                && matches!(
                    &fields[..],
                    [
                        _,
                        Operand::Const(Const::Int(ONE_BYTE_ASCII_W1), _),
                        Operand::Const(Const::Int(0), _)
                    ]
                )
        }
        _ => false,
    }
}

/// The right-hand sides of every write to local `l`: empty (or not only assignments of the
/// whole local) means some write is something else.
fn whole_writes(function: &Function, l: Local) -> Vec<&Rvalue> {
    let mut out = vec![];
    for block in &function.blocks {
        for s in &block.stmts {
            match s {
                Stmt::Assign(place, rv) if place.local == l => {
                    if !place.proj.is_empty() {
                        return vec![];
                    }
                    out.push(rv);
                }
                Stmt::MemCopy { dst, .. }
                | Stmt::MemCopyDyn { dst, .. }
                | Stmt::MemSet { dst, .. }
                    if matches!(dst, Operand::Copy(p) if p.local == l) =>
                {
                    return vec![];
                }
                _ => {}
            }
        }
        if let Terminator::Call { dest: Some(d), .. } = &block.term {
            if d.local == l {
                return vec![];
            }
        }
    }
    out
}

/// How many `&l…` the function takes.
fn addresses_taken(function: &Function, l: Local) -> usize {
    function
        .blocks
        .iter()
        .flat_map(|b| &b.stmts)
        .filter(|s| matches!(s, Stmt::Assign(_, Rvalue::AddrOf(p)) if p.local == l))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use velt_vir::vir::{BasicBlock, Linkage, LocalDecl, StaticId};

    /// `f(s)`: `z = { &static, w1, w2 }; p = &z; velt_rt_str_cmp(s, p)` (or `(p, s)`).
    fn function(w1: i128, w2: i128, literal_first: bool) -> (Function, Vec<Operand>) {
        let local = |ty| LocalDecl { ty, name: None };
        let (s, z, p) = (Local(0), Local(1), Local(2));
        let stmts = vec![
            Stmt::Assign(
                Place::local(z),
                Rvalue::Aggregate(
                    STR_AGG,
                    vec![
                        Operand::Const(Const::Static(StaticId(0)), Ty::Ptr),
                        Operand::Const(Const::Int(w1), Ty::U64),
                        Operand::Const(Const::Int(w2), Ty::U64),
                    ],
                ),
            ),
            Stmt::Assign(Place::local(p), Rvalue::AddrOf(Place::local(z))),
        ];
        let f = Function {
            symbol: "f".into(),
            params: vec![Ty::Ptr],
            ret: Ty::Unit,
            locals: vec![local(Ty::Ptr), local(Ty::Agg(STR_AGG)), local(Ty::Ptr)],
            blocks: vec![BasicBlock {
                stmts,
                term: Terminator::Return(Operand::Const(Const::Unit, Ty::Unit)),
            }],
            linkage: Linkage::Internal,
            locs: vec![],
            param_attrs: vec![],
            is_poll: false,
        };
        let (s, p) = (
            Operand::Copy(Place::local(s)),
            Operand::Copy(Place::local(p)),
        );
        let args = if literal_first {
            vec![p, s]
        } else {
            vec![s, p]
        };
        (f, args)
    }

    fn pick(w1: i128, w2: i128, literal_first: bool) -> Option<&'static str> {
        let (f, args) = function(w1, w2, literal_first);
        literal_compare(&f, "velt_rt_str_cmp", &[Ty::Ptr, Ty::Ptr], Ty::I32, &args)
            .map(|(name, _)| name)
    }

    #[test]
    fn only_one_byte_static_literals_take_the_inline_compare() {
        assert_eq!(
            pick(ONE_BYTE_ASCII_W1, 0, false),
            Some("@velt.str_cmp_byte")
        );
        assert_eq!(pick(ONE_BYTE_ASCII_W1, 0, true), Some("@velt.str_byte_cmp"));
        // Two bytes, a non-ASCII byte count, a heap or inline form: the runtime.
        assert_eq!(pick((2 << 32) | 2, 0, false), None);
        assert_eq!(pick((1 << 32) | 2, 0, false), None);
        assert_eq!(pick(ONE_BYTE_ASCII_W1, 16, false), None);
    }

    #[test]
    fn two_variable_strings_stay_a_call() {
        let (f, _) = function(ONE_BYTE_ASCII_W1, 0, false);
        let args = [
            Operand::Copy(Place::local(Local(0))),
            Operand::Copy(Place::local(Local(0))),
        ];
        let sig = [Ty::Ptr, Ty::Ptr];
        assert!(literal_compare(&f, "velt_rt_str_cmp", &sig, Ty::I32, &args).is_none());
        let (f, args) = function(ONE_BYTE_ASCII_W1, 0, false);
        assert!(literal_compare(&f, "velt_rt_str_eq", &sig, Ty::U8, &args).is_none());
    }

    #[test]
    fn the_literal_on_the_left_negates() {
        let (_, args) = function(ONE_BYTE_ASCII_W1, 0, true);
        let (f, _) = function(ONE_BYTE_ASCII_W1, 0, true);
        let (_, def) =
            literal_compare(&f, "velt_rt_str_cmp", &[Ty::Ptr, Ty::Ptr], Ty::I32, &args).unwrap();
        assert!(def.contains("@velt.str_byte_cmp(ptr %l, ptr %s)"), "{def}");
        assert!(def.contains("%neg = sub i32 0, %r"), "{def}");
        let (_, def) = literal_compare(
            &function(ONE_BYTE_ASCII_W1, 0, false).0,
            "velt_rt_str_cmp",
            &[Ty::Ptr, Ty::Ptr],
            Ty::I32,
            &function(ONE_BYTE_ASCII_W1, 0, false).1,
        )
        .unwrap();
        assert!(def.contains("@velt.str_cmp_byte(ptr %s, ptr %l)"), "{def}");
        assert!(!def.contains("velt_rt_str_cmp"), "{def}");
    }
}
