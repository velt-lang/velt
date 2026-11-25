//! velt/db's API for native drivers end to end (https://veltlang.com/docs/std/db/ "Native drivers"): the test
//! package `native_rows/rows` (a path dependency, its library built from source with cargo) offers
//! a row cursor and a parameter sink, and the program reads rows with `decodeRows` and
//! `decodeRow` (plain fields, a nested-free struct read by column, a class read through the JSON
//! decoder, a row that does not fit, every cell kind through the JSON decoder and as `JsonValue`s,
//! a blob read as numbers, a fractional float refused as an integer, a nested value's own
//! mismatch, unions that look ahead within a row and from the top, which
//! copies the rows out of the cursor) and binds parameters with `bindParams` (an object, an array,
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

const PROGRAM: &str = r#"import {
  all,
  bound,
  closeCursor,
  closedCursor,
  mixed,
  mixedOne,
  one,
  openSink,
  stepped,
  takeSink,
  whole,
} from "rows";
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

interface Meta {
  tags: string[];
  n: f64;
  deep: { k: (bool | null)[] };
}

// The other cell kinds, through the JSON decoder: a boolean, a blob as its bytes, a nested value
// read from its JSON text, text that is not UTF-8, a float that is an integer, a null.
class Mixed {
  ok: bool = false;
  data: u8[] = [];
  meta: Meta = { tags: [], n: 0, deep: { k: [] } };
  raw: string = "";
  whole: i64 = 0;
  maybe: string | null = null;
}

// Columns kept as `JsonValue`s: a nested value, a blob, text.
class Values {
  meta: JsonValue = JsonValue.of(null);
  data: JsonValue = JsonValue.of(null);
  raw: JsonValue = JsonValue.of(null);
}

// A nested value of the wrong shape: its own mismatch, at its path.
struct BadMeta {
  meta: { tags: f64[] };
}

// A blob read as numbers (each byte through the reader's number path).
class BlobNumbers {
  data: f64[] = [];
}

// A float cell that is not an integer, read into an integer field (the `JsonValue` field sends
// the row through the JSON decoder): a mismatch at its path.
class ScoreAsInteger {
  id: i64 = 0;
  score: i64 = 0;
  note: JsonValue = JsonValue.of(null);
}

// A union told apart by a column after the first (`score`, which only one member requires):
// the decoder looks ahead in the row.
type Either = { id: i64; score: f64 } | { id: i64; missing: string };

// The same from the top, the copy's text cell kept as a `JsonValue` (written as JSON text).
type Kept = { id: i64; missing: string } | { id: i64; name: JsonValue };

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
  // Rows read as `JsonValue`s (std's rows reader, through the JSON decoder).
  const values = all<JsonValue>(2);
  console.log(JSON.stringify(values), values[1].get("note")!.asString());
  console.log(one<JsonValue>());
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
  for (const m of mixed<Mixed>(3)) {
    console.log(m.ok, m.data, m.meta.tags, m.meta.n, m.meta.deep.k, m.raw, m.whole, m.maybe);
  }
  const lone = mixedOne<Mixed>();
  console.log(lone.data, lone.meta.tags, lone.whole);
  console.log(JSON.stringify(mixed<JsonValue>(2)));
  console.log(mixedOne<JsonValue>());
  for (const v of mixed<Values>(2)) {
    console.log(JSON.stringify(v.meta), JSON.stringify(v.data), JSON.stringify(v.raw));
  }
  for (const b of mixed<BlobNumbers>(3)) {
    console.log(b.data);
  }
  try {
    all<ScoreAsInteger>(2);
  } catch (e) {
    console.log(e instanceof RowError, e.message);
  }
  try {
    mixed<BadMeta>(2);
  } catch (e) {
    console.log(e instanceof RowError, e.message);
  }
  for (const u of all<Either>(3)) {
    console.log(JSON.stringify(u));
  }
  // A union of whole results looks ahead from the top: the row is copied out of the cursor first.
  console.log(JSON.stringify(one<Either>()));
  console.log(JSON.stringify(one<Kept>()));
  console.log(JSON.stringify(whole<Row[]>(2)));
  console.log(stepped<JsonValue>(3), stepped<Row>(3));
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
[{\"id\":1,\"name\":\"row 1\",\"score\":1.5,\"note\":null},{\"id\":2,\"name\":\"row 2\",\"score\":3,\"note\":\"even\"}] even
{ id: 1, name: 'row 1', score: 1.5, note: null }
true expected field \"tags\" at $[0]
true expected string at $[0].id
false [ 0 ] [ 't1', 'x' ] 1 [ true, null ] caf� \"q\" 2 null
true [ 0, 7 ] [ 't2', 'x' ] 2 [ true, null ] caf� \"q\" 4 set
false [ 0, 7, 255 ] [ 't3', 'x' ] 3 [ true, null ] caf� \"q\" 6 null
[ 0 ] [ 't1', 'x' ] 2
[{\"ok\":false,\"data\":[0],\"meta\":{\"tags\":[\"t1\",\"x\"],\"n\":1,\"deep\":{\"k\":[true,null]}},\"raw\":\"caf� \\\"q\\\"\",\"whole\":2,\"maybe\":null},{\"ok\":true,\"data\":[0,7],\"meta\":{\"tags\":[\"t2\",\"x\"],\"n\":2,\"deep\":{\"k\":[true,null]}},\"raw\":\"caf� \\\"q\\\"\",\"whole\":4,\"maybe\":\"set\"}]
{
  ok: false,
  data: [ 0 ],
  meta: { tags: [ 't1', 'x' ], n: 1, deep: { k: [Array] } },
  raw: 'caf� \"q\"',
  whole: 2,
  maybe: null
}
{\"tags\":[\"t1\",\"x\"],\"n\":1,\"deep\":{\"k\":[true,null]}} [0] \"caf� \\\"q\\\"\"
{\"tags\":[\"t2\",\"x\"],\"n\":2,\"deep\":{\"k\":[true,null]}} [0,7] \"caf� \\\"q\\\"\"
[ 0 ]
[ 0, 7 ]
[ 0, 7, 255 ]
true expected i64 at $[0].score
true expected number at $[0].meta.tags[0]
{\"id\":1,\"score\":1.5}
{\"id\":2,\"score\":3}
{\"id\":3,\"score\":4.5}
{\"id\":1,\"score\":1.5}
{\"id\":1,\"name\":\"row 1\"}
[{\"id\":1,\"name\":\"row 1\",\"score\":1.5,\"note\":null},{\"id\":2,\"name\":\"row 2\",\"score\":3,\"note\":\"even\"}]
3 3
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
        app.join("package.velt"),
        format!(
            "import type {{ Package }} from \"velt/package\";\n\nexport const pkg: Package = {{\n  name: \"app\",\n  version: \"0.1.0\",\n  allowNative: [\"rows\"],\n  dependencies: {{ rows: {{ path: \"{dep}\" }} }},\n}};\n"
        ),
    )
    .unwrap();
    std::fs::write(app.join("src/main.velt"), PROGRAM).unwrap();
    let out = no_window::command(env!("CARGO_BIN_EXE_velt"))
        .args(["run", "src/main.velt"])
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
