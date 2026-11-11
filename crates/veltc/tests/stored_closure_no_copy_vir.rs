//! A spawn helper elsewhere in the program doesn't make every closure of its function type cross
//! (#2756): a closure stored in a field and called on the task that made it, which only reads
//! what it captured, shares its captures with each call. Since #2734 the helper's parameter made
//! `Job` a crossing type for the whole program, and the closure deep-copied what it captured on
//! every call.
//!
//! The check: the VIR of the stored closure's constructor (`main::{closure#2}`) calls no clone
//! glue (`_Gclone_…`).

mod no_window;
mod test_dir;

const SRC: &str = "type Job = () => Promise<void>;
class Box {
  cb: Job | null = null;
}
class Counter {
  n: i64 = 41;
}
async function main() {
  const later = (g: Job): Promise<void> => spawn(g());
  await later(async (): Promise<void> => {
    console.log(\"spawned\");
  });
  const c = new Counter();
  const b = new Box();
  b.cb = async (): Promise<void> => {
    console.log(\"read\", c.n);
  };
  const cb = b.cb;
  if (cb != null) await cb();
}
";

fn vir(src: &str) -> String {
    let dir = test_dir::TestDir::new();
    std::fs::write(dir.path().join("main.velt"), src).unwrap();
    let out = crate::no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["build", "main.velt", "--emit", "vir"])
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

/// The body of the function whose header line contains `name(ptr)`, through its closing brace.
fn body<'a>(v: &'a str, name: &str) -> Vec<&'a str> {
    let mut lines = v
        .lines()
        .skip_while(|l| !(l.starts_with("fn#") && l.contains(name)));
    let mut out = vec![];
    for l in lines.by_ref() {
        out.push(l);
        if l == "}" {
            break;
        }
    }
    out
}

#[test]
fn a_stored_closure_reading_its_captures_shares_them_beside_a_spawn_helper() {
    let v = vir(SRC);
    let ctor = body(&v, "_V4mainN17_7bclosure_232_7d(ptr)");
    assert!(
        !ctor.is_empty(),
        "no constructor of main::{{closure#2}} in:\n{v}"
    );
    let copies: Vec<&&str> = ctor.iter().filter(|l| l.contains("_Gclone_")).collect();
    assert!(
        copies.is_empty(),
        "the stored closure copies what it captured on every call:\n{}",
        ctor.join("\n")
    );
}
