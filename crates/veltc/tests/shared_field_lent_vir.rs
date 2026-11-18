//! A `shared` handle in a field nothing writes is lent to a call without a count (#3113,
//! velt_vir `lower/async_fn/sync.rs` `handle_replaceable`): the call reads it in place. A field
//! some code assigns keeps its count for the statement. The golden `lang/shared_field_lent`
//! checks that what is lent stays readable; this checks that the count is gone.

mod no_window;
mod test_dir;

/// The VIR of function `name` in `src`, compiled as `main.velt`.
fn vir_of(src: &str, name: &str) -> String {
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
    let vir = String::from_utf8_lossy(&out.stdout);
    // The definition (`fn#3 internal _V5fixed(ptr) -> …`), not a call of it.
    let start = vir
        .lines()
        .find(|l| l.starts_with("fn#") && l.contains(&format!("{name}(")))
        .and_then(|l| vir.find(l))
        .unwrap_or_else(|| panic!("no {name} in {vir}"));
    let body = &vir[start..];
    body[..body.find("\n}").unwrap_or(body.len())].to_string()
}

const SRC: &str = "
type Fixed = { rows: shared<f32[]>; n: usize };
type Swapped = { cells: shared<f32[]>; n: usize };

class Table {
  rows: shared<f32[]>;

  constructor(rows: f32[]) {
    this.rows = shared(rows);
  }

  total(): f32 {
    let s: f32 = 0;
    for (let k = 0; k < 3; k++) {
      s += sum(this.rows, 2);
    }
    return s;
  }
}

function sum(xs: f32[], n: usize): f32 {
  let s: f32 = 0;
  for (let i: usize = 0; i < n; i++) {
    s += xs[i];
  }
  return s;
}

function fixed(f: Fixed): f32 {
  let s: f32 = 0;
  for (let k = 0; k < 3; k++) {
    s += sum(f.rows, f.n);
  }
  return s;
}

function swapped(w: Swapped): f32 {
  let s: f32 = 0;
  for (let k = 0; k < 3; k++) {
    s += sum(w.cells, w.n);
  }
  return s;
}

function main() {
  const f: Fixed = { rows: shared([1 as f32, 2]), n: 2 };
  const w: Swapped = { cells: shared([3 as f32, 4]), n: 2 };
  console.log(fixed(f), swapped(w), new Table([5, 6]).total());
  w.cells = shared([7 as f32, 8]);
  console.log(swapped(w));
}
";

/// A count is a clone or share glue call; a release, a drop glue call.
fn counts(body: &str) -> usize {
    body.matches("_Gclone").count()
        + body.matches("_Gshare").count()
        + body.matches("_Gdrop").count()
}

#[test]
fn a_field_nothing_writes_is_lent_without_a_count() {
    for name in ["fixed", "total"] {
        let body = vir_of(SRC, name);
        assert_eq!(counts(&body), 0, "{body}");
    }
}

#[test]
fn a_field_some_code_assigns_keeps_its_count() {
    let body = vir_of(SRC, "swapped");
    assert!(body.contains("_Gclone"), "{body}");
    assert!(body.contains("_Gdrop"), "{body}");
}
