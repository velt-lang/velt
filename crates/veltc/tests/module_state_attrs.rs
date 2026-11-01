//! Borrowed params that module state can reach get neither `frozen` nor `noalias`
//! (velt_vir param_attrs.rs "Module state"): a callee may write the argument through the module
//! variable or static field holding it. `tests/golden/lang/frozen_module_state.vlt` checks that
//! such programs print what Node prints; this checks the attributes themselves, including the
//! `noalias` case whose wrong answer LLVM does not produce today (#1631), and that params of
//! types only locals reach keep them.

use veltc::cli::Emit;
use veltc::driver::{self, BuildOptions, Session};

/// The VIR (before `velt_opt`) of the program `src`, written to a fresh file.
fn vir(name: &str, src: &str) -> String {
    let dir = std::env::temp_dir().join(format!("velt-module-state-attrs-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join(format!("{name}.vlt"));
    std::fs::write(&input, src).unwrap();
    let opts = BuildOptions {
        input,
        emit: Emit::Vir,
        ..Default::default()
    };
    let mut sess = Session::new();
    match driver::compile(&mut sess, &opts) {
        Ok(program) => program.to_string(),
        Err(e) => panic!("{e:?}\n{}", sess.render_diagnostics()),
    }
}

/// The printed first param of the function whose symbol contains `func`.
fn first_param<'v>(vir: &'v str, func: &str) -> &'v str {
    let mut lines = vir.lines();
    lines
        .find(|l| l.starts_with("fn#") && l.contains(&format!("{func}(")))
        .unwrap_or_else(|| panic!("no function {func} in\n{vir}"));
    lines
        .next()
        .filter(|l| l.trim_start().starts_with("param _0"))
        .unwrap_or_else(|| panic!("{func} has no param"))
}

const CLASS_AND_LOOP: &str = "
class Cfg { limit: i64 = 3; }
function count(c: Cfg): i64 {
  let n = 0;
  while (n < c.limit) { n++; if (n == 2) { bump(); } }
  return n;
}
function reset(c: Cfg): i64 { c.limit = 1; bump(); return c.limit; }
";

#[test]
fn params_module_state_reaches_are_not_frozen_or_noalias() {
    for (name, state, bump, arg) in [
        (
            "module_const",
            "const cfg = new Cfg();",
            "cfg.limit = 10;",
            "cfg",
        ),
        (
            "module_let",
            "let cfg = new Cfg();",
            "cfg.limit = 10;",
            "cfg",
        ),
        (
            "static_field",
            "class R { static inst: Cfg = new Cfg(); }",
            "R.inst.limit = 10;",
            "R.inst",
        ),
        (
            "array_element",
            "const cfgs: Cfg[] = [new Cfg()];",
            "cfgs[0].limit = 10;",
            "cfgs[0]",
        ),
    ] {
        let src = format!(
            "{CLASS_AND_LOOP}{state}\nfunction bump(): void {{ {bump} }}\n\
             console.log(count({arg}), reset({arg}));\n"
        );
        let v = vir(name, &src);
        let count = first_param(&v, "count");
        assert!(!count.contains("frozen"), "{name}: {count}");
        // `readonly` covers only writes through the param itself: it stays.
        if name == "module_const" {
            assert!(count.contains("readonly"), "{count}");
        }
        let reset = first_param(&v, "reset");
        assert!(!reset.contains("noalias"), "{name}: {reset}");
    }
}

#[test]
fn params_only_locals_reach_keep_frozen_and_noalias() {
    let src = format!(
        "{CLASS_AND_LOOP}let calls = 0;\nfunction bump(): void {{ calls++; }}\n\
         function main() {{ const c = new Cfg(); console.log(count(c), reset(c), calls); }}\n"
    );
    let v = vir("locals_only", &src);
    let count = first_param(&v, "count");
    assert!(count.contains("frozen"), "{count}");
    let reset = first_param(&v, "reset");
    assert!(reset.contains("noalias"), "{reset}");
}

#[test]
fn a_module_interface_value_reaches_its_implementations() {
    let src = "
interface Limited { limit: i64; get(): i64; }
class Cfg implements Limited { limit: i64 = 3; get(): i64 { return this.limit; } }
const held: Limited = new Cfg();
function count(c: Cfg): i64 {
  let n = 0;
  while (n < c.limit) { n++; if (n == 2) { held.limit = 10; } }
  return n;
}
function main() { const c = new Cfg(); console.log(count(c)); }
";
    let v = vir("interface_value", src);
    let count = first_param(&v, "count");
    assert!(!count.contains("frozen"), "{count}");
}
