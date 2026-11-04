//! Less common M2 paths: rest patterns, `catch {}` without a binding, class
//! objects as interface values, generic impls, virtual methods taking owned arguments (thunks),
//! printing tuples, hashing structured values. The programs that print numbers or index arrays
//! are Velt source loaded with the real std (std_src.rs): both go through std's hooks (#2197).

use velt_sema::hir::{AdtKind, Def, PassMode, UseMode as U};

use super::builder::*;
use super::builder_m2::*;
use super::{run, run_src};

#[test]
fn rest_pattern_and_catch_without_binding() {
    let src = "class Boom {\n  message: string;\n  constructor(message: string) {\n    \
               this.message = message;\n  }\n}\n\nfunction boom() {\n  throw new Boom(\"x\" + \"y\");\n}\n\n\
               function main() {\n  const [a, ...rest] = [\"p\" + \"1\", \"q\", \"r\" + \"\"];\n  \
               console.log(a, rest);\n  try {\n    boom();\n  } catch {\n    console.log(\"caught\");\n  }\n}\n";
    let out = run_src(src);
    assert_eq!(out.stdout, "p1 [ 'q', 'r' ]\ncaught\n");
}

/// A `Base` holding a `Sub` calls `take` (owned argument) virtually, and as a `Shows` interface
/// value it calls `show`, which calls `take` virtually again.
#[test]
fn classes_as_interface_values_and_owned_virtual_args() {
    let src = "interface Shows {\n  show(): string;\n}\n\nclass Base implements Shows {\n  \
               n: i64 = 0;\n  take(x: string): string {\n    const keep = x;\n    return \"base \" + keep;\n  }\n  \
               show(): string {\n    return this.take(\"via dyn\");\n  }\n}\n\n\
               class Sub extends Base {\n  tag: string = \"t\";\n  override take(x: string): string {\n    \
               const keep = x;\n    return \"sub \" + keep;\n  }\n}\n\n\
               function main() {\n  const b: Base = new Sub();\n  const arg = \"o\" + \"wned\";\n  \
               console.log(b.take(arg));\n  const d: Shows = b;\n  console.log(d.show(), b);\n}\n";
    let out = run_src(src);
    assert_eq!(
        out.stdout,
        "sub owned\nsub via dyn Sub { n: 0, tag: 't' }\n"
    );
}

/// Prints a generic `Box` through an interface method called on a type parameter and as a value
/// next to a tuple, and looks up two equal `Box`es and another one as `Map` keys: `kind` is
/// `struct` (a declared struct, which hashes its fields) or `class` (which hashes its identity).
fn structured_hash_print(kind: &str) -> String {
    let ctor = match kind {
        "class" => "\n  constructor(v: T) {\n    this.v = v;\n  }",
        _ => "",
    };
    let make = |v: &str| match kind {
        "class" => format!("new Box<string>({v})"),
        _ => format!("Box {{ v: {v} }}"),
    };
    let src = format!(
        "interface Sized {{\n  size(): i64;\n}}\n\n{kind} Box<T> implements Sized {{\n  v: T;{ctor}\n  \
         size(): i64 {{\n    return 1;\n  }}\n}}\n\nfunction count<T extends Sized>(x: T): i64 {{\n  \
         return x.size();\n}}\n\nfunction main() {{\n  const empty = \"\";\n  const bx = {};\n  \
         console.log(count(bx));\n  const tup: [i64, string] = [1, \"a\"];\n  console.log(tup, bx);\n  \
         const m = new Map<Box<string>, i64>();\n  m.set(bx, 1);\n  \
         console.log(m.has({}), m.has({}));\n}}\n",
        make("empty + \"k\""),
        make("empty + \"k\""),
        make("empty + \"j\""),
    );
    run_src(&src).stdout
}

#[test]
fn generic_impl_and_structured_hash_print() {
    // An object hashes its identity (`Map` keys, #1257): an equal object is another key.
    let out = structured_hash_print("class");
    assert_eq!(out, "1\n[ 1, 'a' ] Box { v: 'k' }\nfalse false\n");
}

#[test]
fn declared_struct_hashes_its_content() {
    // A type declared with `struct` hashes its fields (`Map` keys, #641): an equal struct is
    // the same key.
    let out = structured_hash_print("struct");
    assert_eq!(out, "1\n[ 1, 'a' ] Box { v: 'k' }\ntrue false\n");
}

/// `dispose()` hooks run before the fields drop: on scope exit, for class objects, and for
/// array elements (in element order).
#[test]
fn dispose_hooks() {
    let mut pb = PB::new();
    let t = pb.t;
    let (hd, rd) = (pb.declare(), pb.declare());
    let (ht, rt) = (pb.adt_ty(hd, vec![]), pb.adt_ty(rd, vec![]));
    let dispose = |pb: &mut PB, name: &str, ty, fty, label: &str| {
        let mut f = FB::method(name, ty, t.unit);
        let this = f.param("this", ty, PassMode::BorrowMut);
        let body = vec![se(print(
            vec![s(label, t), field(f.bw(this), 0, U::Borrow, fty)],
            t,
        ))];
        pb.add_fn(f.build(body))
    };
    let hdisp = dispose(&mut pb, "Handle.dispose", ht, t.i64, "dispose handle");
    let rdisp = dispose(&mut pb, "Res.dispose", rt, t.str, "dispose res");
    let mut h = adt("Handle", AdtKind::Struct, vec![("id", t.i64, None)]);
    h.dispose = Some(hdisp);
    pb.set_def(hd, Def::Adt(h));
    let mut r = adt("Res", AdtKind::Class, vec![("name", t.str, None)]);
    r.dispose = Some(rdisp);
    pb.set_def(rd, Def::Adt(r));
    let ha = pb.arr(ht);
    let mut f = FB::new("main", t.unit);
    let a = f.local("a", ht);
    let r = f.local("r", rt);
    let xs = f.local("xs", ha);
    let hl = |v| adt_lit(hd, vec![int(v, t.i64)], ht);
    let body = vec![
        let_(a, hl(1)),
        sblock(vec![let_(r, adt_lit(rd, vec![s("x", t)], rt))]),
        let_(xs, array(vec![hl(2), hl(3)], ha)),
        se(print(vec![s("end", t)], t)),
    ];
    pb.add_main(f.build(body));
    let out = run(&pb.finish());
    assert_eq!(
        out.stdout,
        "dispose res x\nend\ndispose handle 2\ndispose handle 3\ndispose handle 1\n"
    );
}
