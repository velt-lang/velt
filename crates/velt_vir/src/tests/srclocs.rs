//! `lower_with` + a source map: statement locations (vir.rs invariant 8) and the locations in
//! panic and uncaught-error messages. `lower` without one produces none of it.

use velt_common::{FileId, SourceMap, Span};
use velt_sema::hir::{AdtKind, BinOp as B, Def, Program};

use super::builder::*;
use super::builder_m2::*;
use super::interp;
use crate::vir::SrcLoc;
use crate::LowerOptions;

const SRC: &str = "  let z\n  print before\n  print 10 / z\n  print after\n";

/// Point statement `i` of `main`'s body at line `i + 1`, column 3 of [`SRC`].
fn place_statements(p: &mut Program) {
    let main = p.entry.expect("entry");
    let Def::Fn(f) = &mut p.defs[main.0 as usize] else {
        panic!("main is not a function")
    };
    let starts: Vec<u32> = std::iter::once(0)
        .chain(SRC.match_indices('\n').map(|(i, _)| i as u32 + 1))
        .collect();
    for (i, s) in f.body.block.stmts.iter_mut().enumerate() {
        let (lo, hi) = (starts[i] + 2, starts[i + 1] - 1);
        s.span = Span::new(FileId(0), lo, hi);
    }
}

fn source_map() -> SourceMap {
    let mut sm = SourceMap::new();
    sm.add("dir\\t.vlt", SRC);
    sm
}

fn lower_located(p: &Program, sm: &SourceMap) -> crate::vir::Program {
    let opts = LowerOptions {
        source_map: Some(sm),
        std_root: None,
        native_inits: &[],
        cell_checks: false,
    };
    let v = crate::lower_with(p, &opts);
    if let Err(errs) = crate::verify(&v) {
        panic!("verify failed:\n{}\n\n{v}", errs.join("\n"));
    }
    v
}

fn division_program() -> Program {
    let mut pb = PB::new();
    let t = pb.t;
    let mut f = FB::new("main", t.unit);
    let z = f.local("z", t.i64);
    let body = vec![
        let_(z, int(0, t.i64)),
        se(print(vec![s("before", t)], t)),
        se(print(vec![bin(B::Div, int(10, t.i64), f.cp(z))], t)),
        se(print(vec![s("after", t)], t)),
    ];
    pb.add_main(f.build(body));
    let mut p = pb.finish();
    place_statements(&mut p);
    p
}

#[test]
fn division_by_zero_names_its_location() {
    let p = division_program();
    let v = lower_located(&p, &source_map());
    assert_eq!(v.files, ["dir/t.vlt"]);
    let out = interp::run(&v);
    assert_eq!(out.stdout, "before\n");
    assert_eq!(out.stderr, "panic: division by zero at dir/t.vlt:3:3\n");
    assert_eq!(out.code, 101);
}

#[test]
fn every_statement_records_a_location() {
    let v = lower_located(&division_program(), &source_map());
    let main = v
        .funcs
        .iter()
        .find(|f| f.symbol == "_V4main")
        .expect("main");
    assert_eq!(main.locs.len(), main.blocks.len());
    let lines: Vec<u32> = main
        .locs
        .iter()
        .flatten()
        .flatten()
        .map(|l| l.line)
        .collect();
    for line in [1, 2, 3, 4] {
        assert!(
            lines.contains(&line),
            "no statement on line {line}: {lines:?}"
        );
    }
    assert_eq!(
        main.first_loc(),
        Some(SrcLoc {
            file: 0,
            line: 1,
            col: 3
        })
    );
}

#[test]
fn plain_lower_has_no_locations() {
    let v = crate::lower(&division_program());
    assert!(v.files.is_empty());
    assert!(v.funcs.iter().all(|f| f.locs.is_empty()));
    assert_eq!(interp::run(&v).stderr, "panic: division by zero\n");
}

#[test]
fn uncaught_error_names_the_throw() {
    let mut pb = PB::new();
    let t = pb.t;
    let ed = pb.declare();
    let et = pb.adt_ty(ed, vec![]);
    pb.set_def(
        ed,
        Def::Adt(adt(
            "BadThing",
            AdtKind::Struct,
            vec![("message", t.str, None)],
        )),
    );
    let mut f = FB::new("main", t.unit);
    f.throws = Some(et);
    let body = vec![
        se(print(vec![s("before", t)], t)),
        se(throw(adt_lit(ed, vec![s("oh no", t)], et), t.never)),
    ];
    pb.add_main(f.build(body));
    let mut p = pb.finish();
    place_statements(&mut p);
    let out = interp::run(&lower_located(&p, &source_map()));
    assert_eq!(
        (out.stdout.as_str(), out.stderr.as_str(), out.code),
        ("before\n", "Uncaught BadThing: oh no at dir/t.vlt:2:3\n", 1)
    );
}
