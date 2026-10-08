//! The clone glue of a shared class looks up copies made during a transfer (velt_rt
//! `transfer_map`) only when a transfer can reach that class. Spawning a closure, passing an
//! interface value or a class hierarchy to a task must not switch the lookup on for unrelated
//! shared classes, whose `.clone()` would pay a runtime call outside any transfer (#527).

mod no_window;
mod test_dir;

/// The VIR of `src`, compiled as `main.vlt`.
fn vir_of(src: &str) -> String {
    let dir = test_dir::TestDir::new();
    std::fs::write(dir.path().join("main.vlt"), src).unwrap();
    let out = crate::no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["build", "main.vlt", "--emit", "vir"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The functions of `vir` whose bodies call `velt_rt_xfer_find`.
fn lookups(vir: &str) -> Vec<String> {
    let mut current = String::new();
    let mut out = Vec::new();
    for line in vir.lines() {
        if line.starts_with("fn#") {
            current = line.to_string();
        } else if line.contains("velt_rt_xfer_find(") && !out.contains(&current) {
            out.push(current.clone());
        }
    }
    out
}

const SHARED_POINTS: &str = "
class Pt {
  x: number = 0;
}

class Shape {
  pts: Pt[] = [];
}

function cloneMany(): number {
  const s = new Shape();
  const keep: Pt[] = [];
  for (let i = 0; i < 4; i++) {
    const p = new Pt();
    s.pts.push(p);
    keep.push(p);
  }
  return s.clone().pts.length + keep.length;
}
";

#[test]
fn a_spawned_closure_looks_up_only_its_captures() {
    let vir = vir_of(&format!(
        "{SHARED_POINTS}
class Config {{
  limit: number = 3;
}}

async function main() {{
  const cfg = new Config();
  const job = async (): Promise<number> => cfg.limit;
  console.log(await spawn(job()), cloneMany(), cfg.limit);
}}
"
    ));
    let found = lookups(&vir);
    assert!(
        found.iter().any(|f| f.contains("Config")),
        "the captured class looks up copies: {found:?}"
    );
    assert!(
        !found.iter().any(|f| f.contains("objclone_2Pt")),
        "an unrelated shared class looks up copies: {found:?}"
    );
}

#[test]
fn interfaces_and_hierarchies_look_up_only_what_they_hold() {
    let vir = vir_of(&format!(
        "{SHARED_POINTS}
class Animal {{
  name: string = \"a\";
  speak(): string {{
    return \"...\";
  }}
}}

class Dog extends Animal {{
  override speak(): string {{
    return \"woof\";
  }}
}}

interface Handler {{
  handle(n: number): number;
}}

class Echo implements Handler {{
  handle(n: number): number {{
    return n;
  }}
}}

async function serve(h: Handler, a: Animal): Promise<number> {{
  return h.handle(a.speak().length);
}}

async function main() {{
  const h: Handler = new Echo();
  const a: Animal = new Dog();
  console.log(await spawn(serve(h, a)), cloneMany(), h.handle(1), a.speak());
}}
"
    ));
    let found = lookups(&vir);
    assert!(
        found.iter().any(|f| f.contains("Dog")) && found.iter().any(|f| f.contains("Echo")),
        "the subclass and the implementor look up copies: {found:?}"
    );
    assert!(
        !found.iter().any(|f| f.contains("objclone_2Pt")),
        "an unrelated shared class looks up copies: {found:?}"
    );
}
