//! Shape dictionaries (docs/internals/design/shape-sharing.md, section 2).

use crate::vir::{Callee, Const, Operand, Program, StaticId, Terminator};
use crate::LowerOptions;

/// A lock set (a `shared` `RwLock`) and views read inside a `read` callback.
const READ_VIEWS: &str = "
type A = { a: number; b: string };
type B = { c: string; d: number };
class Box {
  items: A[];
  constructor(items: A[]) {
    this.items = items;
  }
}
const lock = shared(new RwLock<Box>(new Box([{ a: 1, b: \"x\" }])));
const ra: any = [{ a: 1, b: \"x\" }];
const rb: any = [{ c: \"y\", d: 2 }];
const n = lock.read((bx) => {
  const va = ra as A[];
  const vb = rb as B[];
  return va[0].a + vb[0].d + bx.items.length;
});
console.log(n);
";

/// `src` lowered with shape sharing on, and verified.
fn lower_shared(src: &str) -> Program {
    let hir = super::std_src::check_src(src);
    let opts = LowerOptions {
        shapes: true,
        ..Default::default()
    };
    let v = crate::lower_with(&hir, &opts);
    if let Err(errs) = crate::verify(&v) {
        panic!("verify failed:\n{}", errs.join("\n"));
    }
    v
}

/// Every call that passes a dictionary: (the callee's symbol, the dictionary).
fn dict_calls(v: &Program) -> Vec<(String, StaticId)> {
    let mut out = vec![];
    for f in &v.funcs {
        for b in &f.blocks {
            let Terminator::Call {
                callee: Callee::Func(g),
                args,
                ..
            } = &b.term
            else {
                continue;
            };
            for a in args {
                if let Operand::Const(Const::Static(s), _) = a {
                    let st = &v.statics[s.0 as usize];
                    let header = st.relocs.first().is_some_and(|r| r.0 == 0);
                    if header && st.bytes.len() >= 24 {
                        out.push((v.funcs[g.0 as usize].symbol.clone(), *s));
                    }
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// The lock mode a function was built for, from its symbol's suffix (lower/program.rs): `R`
/// (read), `K` (checked), `D` (deferred), or empty (plain).
fn mode(sym: &str) -> &str {
    match sym.rsplit_once('_') {
        Some((_, m @ ("R" | "K" | "D"))) => m,
        _ => "",
    }
}

/// A dictionary's functions are built in the mode of the shared instance it is passed to
/// (`push_dict_entry`, `func_in_mode`): a read-mode instance calls read-mode entries, whose
/// counts follow read mode's rules. Built in another mode, the entries run but count as plain
/// code does, and no program output shows it.
#[test]
fn a_dictionary_names_entries_in_its_instances_mode() {
    let v = lower_shared(READ_VIEWS);
    let mut read_entries = 0;
    for (inst, d) in dict_calls(&v) {
        for (_, c) in &v.statics[d.0 as usize].relocs {
            let Const::Func(f) = c else { continue };
            let entry = &v.funcs[f.0 as usize].symbol;
            assert_eq!(
                mode(entry),
                mode(&inst),
                "`{inst}` is passed a dictionary naming `{entry}`"
            );
            read_entries += usize::from(mode(&inst) == "R");
        }
    }
    assert!(
        read_entries > 0,
        "no read-mode instance with code entries:\n{v}"
    );
}
