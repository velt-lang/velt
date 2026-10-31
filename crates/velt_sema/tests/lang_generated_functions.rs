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
    assert_eq!(d.functions.len(), 1);
    assert_eq!(d.functions[0].function, "describe");
    assert_eq!(d.functions[0].generator, "./gen");
    assert_eq!(d.functions[0].ret, "string");
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
