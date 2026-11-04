//! Generated functions (docs/internals/design/source-generators.md): demand through generic
//! code, the type model a generator reads, and what a check that runs no generators reports.

mod common;

use common::programs::{err_src, load_src};
use velt_sema::generated::{Demand, GenInput};
use velt_sema::{CheckOptions, Checked};

const PROGRAM: &str = r#"
declare generated("./gen") function describe<T>(): string;

class User {
  name: string = "";
  private secret: i32 = 0;
  age?: i32;
}

function via<T>(): string {
  return describe<T>();
}

function main() {
  console.log(describe<User>());
  console.log(via<f64[]>());
}
"#;

fn demand(src: &str) -> Demand {
    let l = load_src(src);
    let (out, diags) = velt_sema::check_generating(
        &l.modules,
        l.root,
        CheckOptions::default(),
        &GenInput::default(),
        &[],
        None,
        None,
    );
    assert!(diags.iter().all(|d| !d.is_error()), "{}", l.render(&diags));
    match out {
        Some(Checked::Demand(d)) => d,
        Some(Checked::Program(_)) => panic!("a program without generated code"),
        Some(Checked::Again) => panic!("checked again without a generator"),
        None => panic!("no outcome"),
    }
}

/// A command that runs no generators names the function at each call that needs code.
#[test]
fn checking_without_generators_names_the_function() {
    let r = err_src(PROGRAM);
    assert!(
        r.contains("`describe` is a generated function, and this command does not run generators"),
        "{r}"
    );
}

/// The instance called directly and the one reached through a generic function are both
/// requested, the second at the call of the generic function.
#[test]
fn demand_reaches_instances_through_generic_code() {
    let d = demand(PROGRAM);
    // The prelude declares generated functions of its own (velt/lang/equal), which no code here
    // calls.
    let own: Vec<_> = (d.functions.iter())
        .filter(|f| f.generator == "./gen")
        .collect();
    assert_eq!(own.len(), 1);
    assert_eq!(own[0].function, "describe");
    assert_eq!(own[0].ret, "string");
    let mut shown: Vec<String> = (d.requests.iter())
        .map(|r| d.model.types[r.type_args[0] as usize].display.clone())
        .collect();
    shown.sort();
    assert_eq!(shown, ["User", "f64[]"]);
}

/// The model describes fields (privacy, optional) and names types for generated code.
#[test]
fn the_model_describes_fields_and_spells_types() {
    let d = demand(PROGRAM);
    let user = (d.model.types.iter())
        .find(|t| t.display == "User")
        .expect("User in the model");
    assert_eq!(user.kind, "class");
    // Generated code names a type by a handle: its position in an instance's walk.
    assert!(
        user.spelling.starts_with(r#"__type<""#),
        "{}",
        user.spelling
    );
    let fields: Vec<(&str, bool, bool)> = (user.fields.iter())
        .map(|f| (f.name.as_str(), f.private, f.optional))
        .collect();
    assert_eq!(
        fields,
        [
            ("name", false, false),
            ("secret", true, false),
            ("age", false, true)
        ]
    );
    let age = &user.fields[2];
    assert_eq!(d.model.types[age.ty as usize].repr, "i32");
    let arr = (d.model.types.iter())
        .find(|t| t.kind == "array")
        .expect("an array");
    assert!(arr.spelling.starts_with(r#"__type<""#), "{}", arr.spelling);
    assert_eq!(d.model.types[arr.args[0] as usize].repr, "f64");
}

/// A fingerprint covers what a request reaches, whatever else the model holds and in whatever
/// order: the offered code of an earlier build is bound only while it matches.
#[test]
fn fingerprints_cover_the_types_a_request_reaches() {
    let d = demand(PROGRAM);
    let user = d.requests[0].type_args.clone();
    let sig = d.functions[d.requests[0].function].signature();
    let fp = d.model.fingerprint(&user, &sig);
    // The same types in a model of their own (built for another request first).
    let alone = demand(&PROGRAM.replace("console.log(via<f64[]>());", ""));
    let fp_alone = alone.model.fingerprint(&alone.requests[0].type_args, &sig);
    assert_eq!(fp, fp_alone);
    // Positions do not count; a field does.
    let moved = demand(&PROGRAM.replace("class User {", "\n\nclass User {"));
    assert_eq!(
        fp,
        moved.model.fingerprint(&moved.requests[0].type_args, &sig)
    );
    let grown = demand(&PROGRAM.replace("age?: i32;", "age?: i32;\n  email: string = \"\";"));
    assert_ne!(
        fp,
        grown.model.fingerprint(&grown.requests[0].type_args, &sig)
    );
    // Nor is another signature the same request.
    assert_ne!(fp, d.model.fingerprint(&user, "describe(): number"));
}

/// A method that needs instances and is called dynamically (an override through its base
/// class, an interface method through the interface) reaches them through the class types the
/// program mentions; a method only ever called directly needs no such walk.
#[test]
fn demand_reaches_instances_through_dynamic_dispatch() {
    let d = demand(
        r#"
declare generated("./gen") function describe<T>(): string;

class Base<T> {
  show(): string {
    return "base";
  }
}

class Shown<T> extends Base<T> {
  override show(): string {
    return describe<T>();
  }
}

interface Named {
  name(): string;
}

class Tag<T> implements Named {
  name(): string {
    return describe<T[]>();
  }
}

class Direct<T> {
  text(): string {
    return describe<[T, T]>();
  }
}

function main() {
  const b: Base<i32> = new Shown<i32>();
  console.log(b.show());
  const n: Named = new Tag<bool>();
  console.log(n.name());
  console.log(new Direct<string>().text());
}
"#,
    );
    let mut shown: Vec<String> = (d.requests.iter())
        .filter(|r| d.functions[r.function].generator == "./gen")
        .map(|r| d.model.types[r.type_args[0] as usize].display.clone())
        .collect();
    shown.sort();
    assert_eq!(shown, ["[string, string]", "boolean[]", "i32"]);
}

/// The `./gen` instances `src` demands, shown (none when nothing generated is demanded).
fn shown(src: &str) -> Vec<String> {
    let l = load_src(src);
    let (out, diags) = velt_sema::check_generating(
        &l.modules,
        l.root,
        CheckOptions::default(),
        &GenInput::default(),
        &[],
        None,
        None,
    );
    assert!(diags.iter().all(|d| !d.is_error()), "{}", l.render(&diags));
    let d = match out {
        Some(Checked::Demand(d)) => d,
        Some(Checked::Program(_)) => return vec![],
        _ => panic!("no demand"),
    };
    let mut shown: Vec<String> = (d.requests.iter())
        .filter(|r| d.functions[r.function].generator == "./gen")
        .map(|r| d.model.types[r.type_args[0] as usize].display.clone())
        .collect();
    shown.sort();
    shown
}

const CHAIN: &str = r#"
declare generated("./gen") function describe<T>(): string;

function via<T>(): string {
  return describe<T>();
}

function twice<T>(): string {
  return via<T[]>() + via<T>();
}
"#;

/// Generic code that names a generated function demands nothing while no concrete code reaches
/// it; each way concrete code can reach it demands the instances at its types: a call, a
/// module constant, a field default, a closure, a dynamic call, and a concrete call inside
/// generic code nothing calls.
#[test]
fn demand_reaches_instances_only_from_concrete_code() {
    let unreached = format!("{CHAIN}\nfunction main() {{\n  console.log(\"x\");\n}}\n");
    assert_eq!(shown(&unreached), Vec::<String>::new());
    let called = format!("{CHAIN}\nfunction main() {{\n  console.log(twice<i32>());\n}}\n");
    assert_eq!(shown(&called), ["i32", "i32[]"]);
    let constant = format!(
        "{CHAIN}\nconst text = twice<boolean>();\n\nfunction main() {{\n  console.log(text);\n}}\n"
    );
    assert_eq!(shown(&constant), ["boolean", "boolean[]"]);
    let field = format!(
        "{CHAIN}
class Holder<T> {{
  text: string = twice<T>();
}}

         function main() {{
  console.log(new Holder<i16>().text);
}}
"
    );
    assert_eq!(shown(&field), ["i16", "i16[]"]);
    let closure = format!(
        "{CHAIN}\nfunction wrap<T>(): () => string {{\n  return () => via<T>();\n}}\n\n\
         function main() {{\n  console.log(wrap<u8>()());\n}}\n"
    );
    assert_eq!(shown(&closure), ["u8"]);
    let dynamic = format!(
        "{CHAIN}\nclass Base {{\n  name(): string {{\n    return \"b\";\n  }}\n}}\n\n\
         class Box<T> extends Base {{\n  override name(): string {{\n    return twice<T>();\n  }}\n}}\n\n\
         function main() {{\n  const b: Base = new Box<i64>();\n  console.log(b.name());\n}}\n"
    );
    assert_eq!(shown(&dynamic), ["i64", "i64[]"]);
    let inside = format!(
        "{CHAIN}\nfunction never<T>(): string {{\n  return via<string>();\n}}\n\n\
         function main() {{\n  console.log(\"x\");\n}}\n"
    );
    assert_eq!(shown(&inside), ["string"]);
}

/// A function that calls one leading to a generated function at type arguments of which some are
/// concrete and some its own (`rows<T>` calling `pair<T, Rows>`) has an instance of its own and
/// leads to more through its callers: `bench/sqlite/engine.velt`'s `Statement.all<T>` reading rows
/// with `decodeRows<T, Rows>`.
#[test]
fn demand_follows_a_call_with_concrete_and_generic_type_arguments() {
    let src = r#"
declare generated("./gen") function describe<T>(): string;

class Rows {
  n: i32 = 0;
}

function pair<T, S>(): string {
  return describe<T>() + describe<S>();
}

function rows<T>(): string {
  return pair<T, Rows>();
}

function main() {
  console.log(rows<i32>());
}
"#;
    assert_eq!(shown(src), ["Rows", "i32"]);
}
