//! velt/db's API for native drivers end to end (docs/std/db.md "Native drivers"): the test
//! package `native_rows/rows` (a path dependency, its library built from source with cargo) offers
//! a row cursor and a parameter sink, and the program reads rows with `decodeRows` and
//! `decodeRow` (plain fields, a nested-free struct read by column, a class read through the JSON
//! decoder, a row that does not fit) and binds parameters with `bindParams` (an object, an array,
//! a lone value, types that go as JSON instead). A handle is a key the runtime checks: a made-up
//! number, a closed cursor's handle or a sink's handle used as a cursor's throws `RowError` (binds
//! nothing, for a sink) instead of reaching memory.
//!
//! Needs cargo; `VELT_SKIP_NATIVE_E2E=1` skips it.

mod no_window;
mod test_dir;

use std::path::{Path, PathBuf};

fn package() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/native_rows/rows")
}

const PROGRAM: &str = r#"import { all, bound, closeCursor, closedCursor, one, openSink, takeSink } from "rows";
import { bindParams, decodeRow, decodeRows, RowError } from "velt/db";

struct Row {
  id: i64;
  name: string;
  score: f64;
  note: string | null;
}

// Read through the JSON decoder (a class with a nested array field is not read by column).
class Tagged {
  id: f64 = 0;
  name: string = "";
  tags: string[] = [];
}

struct Bad {
  id: string;
}

struct Item {
  id: i64;
  name: string;
  data: u8[];
  ok: bool;
  gone: string | null;
}

function main() {
  for (const r of all<Row>(3)) {
    console.log(r.id, r.name, r.score, r.note);
  }
  console.log(all<Row>(0).length);
  const first = one<Row>();
  console.log(first.id, first.name, first.note);
  try {
    all<Tagged>(1);
  } catch (e) {
    console.log(e instanceof RowError, e.message);
  }
  try {
    all<Bad>(2);
  } catch (e) {
    console.log(e instanceof RowError, e.message);
  }
  console.log(bound<Item>({ id: 7, name: "ada", data: [1, 2], ok: true, gone: null }));
  console.log(bound<i64[]>([1, 2]));
  console.log(bound<(i64 | string)[]>([1, "two"]));
  console.log(bound<f64>(2.5));
  console.log(bound<Tagged>(new Tagged()));
  // Handles are checked: none of these reads or writes memory.
  const forged: u64[] = [0, 1, 4096, 0xdeadbeef, 0x7fffffffffffffff, closedCursor()];
  for (const h of forged) {
    try {
      decodeRows<Row>(h);
    } catch (e) {
      console.log(e instanceof RowError, e.message);
    }
  }
  try {
    decodeRow<Row>(0x100000001);
  } catch (e) {
    console.log(e instanceof RowError, e.message);
  }
  console.log(bindParams<i64[]>(0xdeadbeef, [1]));
  // A sink's handle is no cursor's, and the other way round; the sink stays open.
  const sink = openSink();
  try {
    decodeRows<Row>(sink);
  } catch (e) {
    console.log(e instanceof RowError, e.message);
  }
  console.log(closeCursor(sink), bindParams<i64[]>(sink, [5]), takeSink(sink));
  console.log(bindParams<i64[]>(sink, [6]));
}
"#;

const EXPECTED: &str = "1 row 1 1.5 null
2 row 2 3 even
3 row 3 4.5 null
0
1 row 1 null
true expected field \"tags\" at $[0]
true expected string at $[0].id
object id= int 7 name= text ada data= blob [1, 2] ok= true gone= null
int 1 int 2
json: [1,\"two\"]
float 2.5
json: {\"id\":0,\"name\":\"\",\"tags\":[]}
true not an open row cursor
true not an open row cursor
true not an open row cursor
true not an open row cursor
true not an open row cursor
true not an open row cursor
true not an open row cursor
false
true not an open row cursor
0 true int 5
false
";

#[test]
fn native_rows_and_parameters_go_through_velt_db() {
    if std::env::var_os("VELT_SKIP_NATIVE_E2E").is_some_and(|v| v == "1") {
        eprintln!("skipped: VELT_SKIP_NATIVE_E2E=1");
        return;
    }
    let tmp = test_dir::TestDir::new();
    let app = tmp.path().join("app");
    std::fs::create_dir_all(app.join("src")).unwrap();
    let dep = package().to_string_lossy().replace('\\', "/");
    std::fs::write(
        app.join("package.vlt"),
        format!(
            "import type {{ Package }} from \"velt/package\";\n\nexport const pkg: Package = {{\n  name: \"app\",\n  version: \"0.1.0\",\n  dependencies: {{ rows: {{ path: \"{dep}\" }} }},\n}};\n"
        ),
    )
    .unwrap();
    std::fs::write(app.join("src/main.vlt"), PROGRAM).unwrap();
    let out = no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["run", "src/main.vlt"])
        .current_dir(&app)
        .output()
        .expect("run velt");
    let (stdout, stderr) = (
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        out.status.success(),
        "velt run failed:\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    assert_eq!(stdout, EXPECTED, "stderr:\n{stderr}");
}
