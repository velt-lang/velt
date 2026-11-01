//! `load()` of an `Atomic` of a `shared` value is a borrow (velt_vir `lower/atomic_ref.rs`):
//! reading it in place, binding it with `const` and narrowing it count nothing, so a read costs
//! one atomic load; a value that is kept is counted.

mod no_window;
mod test_dir;

/// The VIR of function `name` in `src`, compiled as `main.vlt`.
fn vir_of(src: &str, name: &str) -> String {
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
    let vir = String::from_utf8_lossy(&out.stdout);
    // The definition (`fn#3 internal _V5bound() -> …`), not a call of it.
    let start = vir
        .lines()
        .find(|l| l.starts_with("fn#") && l.contains(&format!("{name}(")))
        .and_then(|l| vir.find(l))
        .unwrap_or_else(|| panic!("no {name} in {vir}"));
    let body = &vir[start..];
    body[..body.find("\n}").unwrap_or(body.len())].to_string()
}

const SRC: &str = "
interface Config {
  limit: u64;
}
const CFG = new Atomic<shared<Config> | null>(null);

function sum(c: shared<Config>): u64 {
  return c.limit * 2;
}

function bound(): u64 {
  const c = CFG.load(\"acquire\");
  if (c == null) {
    return 0;
  }
  return c.limit + sum(c);
}

function inPlace(): bool {
  return CFG.load() != null;
}

function kept(): shared<Config> | null {
  return CFG.load();
}

function main() {
  CFG.store(shared({ limit: 3 }));
  console.log(bound(), inPlace(), kept() != null);
}
";

/// A count is a clone or share glue call; a release, a drop glue call.
fn counts(body: &str) -> usize {
    body.matches("_Gclone").count()
        + body.matches("_Gshare").count()
        + body.matches("_Gdrop").count()
}

#[test]
fn reads_count_nothing() {
    for f in ["bound", "inPlace"] {
        let body = vir_of(SRC, f);
        assert!(body.contains("velt_atomic_load_64"), "{f}: {body}");
        assert_eq!(counts(&body), 0, "{f}: {body}");
    }
}

#[test]
fn a_kept_value_is_counted() {
    let body = vir_of(SRC, "kept");
    assert!(counts(&body) > 0, "{body}");
}

const OUTLIVE: &str = "
interface C {
  s: string;
}

function k(xs: Atomic<shared<C>>[]): string {
  const c = xs[0].load();
  xs.pop();
  return c.s;
}

function stays(a: Atomic<shared<C>>): string {
  const c = a.load();
  a.store(shared({ s: \"b\" }));
  return c.s;
}

function main() {
  console.log(k([new Atomic<shared<C>>(shared({ s: \"a\" }))]));
  console.log(stays(new Atomic<shared<C>>(shared({ s: \"a\" }))));
}
";

/// A borrowed load whose `Atomic` the block may drop (`xs.pop()`) takes a share; one whose
/// `Atomic` only gets a `store` (which retires the old value) stays borrowed.
#[test]
fn a_dropped_atomic_makes_the_binding_counted() {
    let shares = |b: &str| b.matches("_Gclone").count() + b.matches("_Gshare").count();
    assert!(
        shares(&vir_of(OUTLIVE, "k")) > 0,
        "{}",
        vir_of(OUTLIVE, "k")
    );
    assert_eq!(
        shares(&vir_of(OUTLIVE, "stays")),
        0,
        "{}",
        vir_of(OUTLIVE, "stays")
    );
}
