//! The structured type query of `velt_sema::ide` (`Analysis::type_of`, `view`, `fields`,
//! `declares_method`; docs/internals/contracts/sema_ide.md "Type query").

mod common;

use common::programs::load_src;
use velt_common::{FileId, Span};
use velt_sema::hir::{FloatTy, IntTy};
use velt_sema::ide::{check_for_ide, Analysis, LiteralKind, NamedKind, TypeRef, TypeView};

struct Typed {
    analysis: Analysis,
    file: FileId,
    src: String,
}

impl Typed {
    fn new(src: &str) -> Typed {
        let l = load_src(src);
        let file = l.modules[l.root].file;
        let analysis = check_for_ide(&l.modules, l.root);
        let errors: Vec<_> = analysis
            .diagnostics()
            .iter()
            .filter(|d| d.is_error())
            .collect();
        assert!(errors.is_empty(), "{errors:?}");
        Typed {
            analysis,
            file,
            src: src.to_string(),
        }
    }

    /// The span of the first occurrence of `text` at or after the first occurrence of `after`.
    fn span(&self, after: &str, text: &str) -> Span {
        let from = self.src.find(after).expect("anchor");
        let lo = from + self.src[from..].find(text).expect("text");
        Span::new(self.file, lo as u32, (lo + text.len()) as u32)
    }

    fn type_of(&self, after: &str, text: &str) -> TypeRef {
        let span = self.span(after, text);
        self.analysis
            .type_of(span)
            .unwrap_or_else(|| panic!("no type recorded for `{text}`"))
    }

    fn view(&self, after: &str, text: &str) -> TypeView {
        self.analysis.view(&self.type_of(after, text))
    }
}

const PROGRAM: &str = "class Box<T> {
  value: T;
  label?: string;
  constructor(value: T) { this.value = value; }
  toString(): string { return \"box\"; }
}
class Crate extends Box<i64> {}
type Opts = { host?: string; port: number };
function div(a: i64, b: i64): i64 { return a / b; }
async function later(): Promise<string> { return \"x\"; }
function main() {
  const seven = 7;
  const half = seven / 2;
  const m = new Map<string, string>();
  const got = m.get(\"k\");
  const xs = [1.5, 2.5];
  const len = xs.length;
  const b = new Box<Map<string, i64>>(new Map<string, i64>());
  const c = new Crate(1);
  const o: Opts = { port: 1 };
  const u: string | number = \"s\";
  const pair: [i64, string] = [1, \"a\"];
  const f = (x: i64) => x;
  const p = later();
  const lit: \"up\" | \"down\" = \"up\";
  console.log(div(7, 2), half, got, len, b, c, o, u, pair, f(1), lit, true);
}
";

#[test]
fn primitive_types() {
    let t = Typed::new(PROGRAM);
    assert!(matches!(
        t.view("return a", "a / b"),
        TypeView::Int(IntTy::I64)
    ));
    assert!(matches!(
        t.view("const half", "seven / 2"),
        TypeView::Float(FloatTy::F64)
    ));
    assert!(matches!(t.view("const len", "xs.length"), TypeView::Int(i) if !i.is_signed()));
    assert!(matches!(t.view("lit, true", "true"), TypeView::Bool));
    assert!(matches!(t.view("const m", "\"k\""), TypeView::Str));
}

#[test]
fn only_an_exact_span_has_a_type() {
    let t = Typed::new(PROGRAM);
    let span = t.span("const half", "seven / 2");
    let inner = Span::new(span.file, span.lo, span.hi - 1);
    assert!(t.analysis.type_of(inner).is_none());
    assert!(t.analysis.type_of(span).is_some());
}

#[test]
fn containers_and_nullable() {
    let t = Typed::new(PROGRAM);
    let TypeView::Nullable(inner) = t.view("const got", "m.get(\"k\")") else {
        panic!("Map.get is nullable");
    };
    assert!(matches!(t.analysis.view(&inner), TypeView::Str));
    let TypeView::Map(k, v) = t.view("const m", "new Map<string, string>()") else {
        panic!("a map");
    };
    assert!(matches!(t.analysis.view(&k), TypeView::Str));
    assert!(matches!(t.analysis.view(&v), TypeView::Str));
    let TypeView::Array(e) = t.view("const xs", "[1.5, 2.5]") else {
        panic!("an array");
    };
    assert!(matches!(t.analysis.view(&e), TypeView::Float(FloatTy::F64)));
    let TypeView::Tuple(parts) = t.view("const pair", "[1, \"a\"]") else {
        panic!("a tuple");
    };
    assert_eq!(parts.len(), 2);
    let TypeView::Promise(r) = t.view("const p", "later()") else {
        panic!("a promise");
    };
    assert!(matches!(t.analysis.view(&r), TypeView::Str));
    assert!(matches!(t.view("const f", "(x: i64) => x"), TypeView::Fn));
}

#[test]
fn unions_and_literals() {
    let t = Typed::new(PROGRAM);
    let TypeView::Union(members) = t.view("console.log", "u") else {
        panic!("a union");
    };
    let mut kinds: Vec<String> = members.iter().map(|m| t.analysis.show_type(m)).collect();
    kinds.sort();
    assert_eq!(kinds, ["f64", "string"]);
    let TypeView::Union(lits) = t.view("console.log", "lit") else {
        panic!("a union of literals");
    };
    assert!(lits
        .iter()
        .all(|m| matches!(t.analysis.view(m), TypeView::Literal(LiteralKind::Str))));
}

#[test]
fn named_types_and_their_fields_with_arguments() {
    let t = Typed::new(PROGRAM);
    let b = t.type_of(
        "const b",
        "new Box<Map<string, i64>>(new Map<string, i64>())",
    );
    let TypeView::Named(named) = t.analysis.view(&b) else {
        panic!("a class");
    };
    assert_eq!(
        (
            named.name.as_str(),
            named.kind,
            named.is_std,
            named.args.len()
        ),
        ("Box", NamedKind::Class, false, 1)
    );
    assert_eq!(t.analysis.show_type(&b), "Box<Map<string, i64>>");
    let fields = t.analysis.fields(&b);
    let names: Vec<(&str, bool)> = fields
        .iter()
        .map(|f| (f.name.as_str(), f.optional))
        .collect();
    assert_eq!(names, [("value", false), ("label", true)]);
    assert!(matches!(t.analysis.view(&fields[0].ty), TypeView::Map(..)));
    assert_eq!(t.analysis.show_type(&fields[0].ty), "Map<string, i64>");
    assert!(matches!(
        t.analysis.view(&fields[1].ty),
        TypeView::Nullable(_)
    ));
}

#[test]
fn inherited_fields_are_substituted_and_methods_are_own_or_not() {
    let t = Typed::new(PROGRAM);
    let c = t.type_of("const c", "new Crate(1)");
    let fields = t.analysis.fields(&c);
    assert!(matches!(
        t.analysis.view(&fields[0].ty),
        TypeView::Int(IntTy::I64)
    ));
    assert!(!t.analysis.declares_method(&c, "toString"));
    let b = t.type_of(
        "const b",
        "new Box<Map<string, i64>>(new Map<string, i64>())",
    );
    assert!(t.analysis.declares_method(&b, "toString"));
}

#[test]
fn object_types_are_records() {
    let t = Typed::new(PROGRAM);
    let o = t.type_of("const o", "{ port: 1 }");
    assert!(matches!(t.analysis.view(&o), TypeView::Record));
    let fields: Vec<(String, bool)> = t
        .analysis
        .fields(&o)
        .into_iter()
        .map(|f| (f.name, f.optional))
        .collect();
    // `{ host?: string }` keeps the `?` (#463: `JSON.stringify` leaves the field out when null).
    assert_eq!(
        fields,
        [("host".to_string(), true), ("port".to_string(), false)]
    );
}

#[test]
fn generic_parameters_are_named() {
    let src = "function first<T>(xs: T[]): T | null { return xs.length > 0 ? xs[0] : null; }
function main() { console.log(first([1])); }
";
    let t = Typed::new(src);
    let TypeView::Param(name) = t.view("? xs", "xs[0]") else {
        panic!("a generic parameter");
    };
    assert_eq!(name, "T");
}

#[test]
fn sets_and_std_classes() {
    let src = "import { Set } from \"velt:collections/set\";
function main() {
  const s = new Set<string>();
  const d = new Date(0);
  console.log(s, d);
}
";
    let t = Typed::new(src);
    let TypeView::Set(e) = t.view("const s", "new Set<string>()") else {
        panic!("a set");
    };
    assert!(matches!(t.analysis.view(&e), TypeView::Str));
    let TypeView::Named(date) = t.view("const d", "new Date(0)") else {
        panic!("a class");
    };
    assert_eq!((date.name.as_str(), date.is_std), ("Date", true));
}

/// A destructuring with defaults reads the fields through synthetic expressions with the
/// destructured value's span; the value's own type is the one reported.
#[test]
fn a_destructured_value_keeps_its_own_type() {
    let src = "type Pt = { x: f64 | null; y: string };
function main() {
  const p: Pt = { x: null, y: \"a\" };
  const { x = 1.0, y } = p;
  console.log(x, y);
}
";
    let t = Typed::new(src);
    let p = t.type_of("} = p", "p");
    assert!(matches!(t.analysis.view(&p), TypeView::Record));
}

#[test]
fn field_only_interfaces_have_their_fields() {
    let t = Typed::new(
        "interface User { name: string; nick?: string }\n\
         function f(u: User): string { return u.name; }\n",
    );
    let u = t.type_of(" u.name", "u");
    let fields: Vec<(String, bool)> = t
        .analysis
        .fields(&u)
        .into_iter()
        .map(|f| (f.name, f.optional))
        .collect();
    assert_eq!(
        fields,
        [("name".to_string(), false), ("nick".to_string(), true)]
    );
}

#[test]
fn def_of_takes_an_exact_span() {
    let t = Typed::new("function f(count: i64): i64 { return count + 1; }\n");
    let at = t.span("return", "count");
    let d = t.analysis.def_of(at).expect("a definition");
    assert_eq!(d.name, "count");
    assert_eq!(Some(d), t.analysis.def_at(at.file, at.lo));
    let wider = Span::new(at.file, at.lo, at.hi + 1);
    assert!(t.analysis.def_of(wider).is_none());
}
