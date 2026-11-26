//! `@own()` (std/lang/intrinsics/decorators.velt, #3792): an instance member of a std class that
//! JavaScript installs on every instance, so a subclass member of the name is never reached and
//! overrides nothing (only std may import the intrinsics).

mod common;

/// `src` checked as std code; the rendered diagnostics (empty when it checks).
fn std_diagnostics(src: &str) -> String {
    let mut l = common::programs::load_src(src);
    let root = l.root;
    l.modules[root].is_std = true;
    let (_, d) = l.check();
    l.render(&d)
}

const IMPORT: &str = "import { own } from \"velt/lang/intrinsics/decorators\";";

fn std_check(body: &str) -> String {
    std_diagnostics(&format!("{IMPORT}\n{body}"))
}

const BASE: &str =
    "class Base {\n  @own()\n  get v(): string | undefined { return \"base\"; }\n}\n";

#[test]
fn a_subclass_member_of_the_name_overrides_nothing() {
    // Members TypeScript accepts (the type narrowed from the base's), which Velt would reject
    // for their signature if they were overrides.
    for sub in [
        "class Sub extends Base { get v(): string { return \"sub\"; } }",
        "class Sub extends Base { override get v(): string { return \"sub\"; } }",
        "class Sub extends Base { get v(): string { return \"sub\"; }\n  set v(x: string | undefined) {} }",
        "class Mid extends Base {}\nclass Sub extends Mid { override get v(): string { return \"sub\"; } }",
    ] {
        let src = format!("{BASE}{sub}\nfunction main() {{ const s: string | undefined = new Sub().v; console.log(s); }}");
        let d = std_check(&src);
        assert!(d.is_empty(), "{sub}:\n{d}");
    }
}

#[test]
fn without_own_the_subclass_member_overrides() {
    let src = "class Base {\n  get v(): string | undefined { return \"base\"; }\n}\nclass Sub extends Base { get v(): string { return \"sub\"; } }\nfunction main() { console.log(new Sub().v); }";
    let d = std_check(src);
    assert!(d.contains("does not have the same signature"), "{d}");
}

#[test]
fn own_applies_to_an_instance_member_of_a_class() {
    let fun = std_check("@own()\nfunction f(): number { return 1; }\nfunction main() {}");
    assert!(
        fun.contains("the decorator `own` cannot be applied to a function"),
        "{fun}"
    );
    let stat =
        std_check("class C {\n  @own()\n  static s(): number { return 1; }\n}\nfunction main() {}");
    assert!(
        stat.contains("`@own` applies to an instance member of a class declared in std"),
        "{stat}"
    );
}
