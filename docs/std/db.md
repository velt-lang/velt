# velt/db

`import { CellKind, readRow, readRows, RowSource, RowError, decodeRows, decodeRow, bindParams }
from "velt/db"`. What database drivers share: typed rows from any source of tabular data, and a
statement's parameters handed to a native driver. A driver written in Velt (a database's wire protocol parsed in
place, a file format) implements `RowSource`, and `readRows<T>` fills one `T` per row: each
column fills the field of its name, as `JSON.parse<T>` fills fields from keys, with the same
conversions (an integer into an `f64` field, an exact float into an integer field, `null` into a
`T | null` field) and the same messages for a row that doesn't fit. The `sqlite` and `postgres`
packages read their rows the same way (below).

- `interface RowSource`: `columnCount(): i64`, `columnName(i): string`, `next(): bool` (to the
  next row, the first on the first call; false when there is none), `kind(i): CellKind` (what
  column `i` of the current row holds), and one getter per kind: `bool(i)`, `int(i): i64`,
  `float(i): f64`, `text(i): string`, `blob(i): u8[]`, `json(i): string` (a nested value as
  JSON text). The reader asks a value's kind before taking it with that kind's getter.
- `enum CellKind { Null, Bool, Int, Float, Text, Blob, Json }`.
- `readRows<T, S extends RowSource>(src: S): T[]`: every remaining row.
- `readRow<T, S extends RowSource>(src: S): T | null`: the next row, or null.
- `RowError { message }`: a row that doesn't fit `T` (`expected i64 at $[0].id`, `expected
  field "name" at $[1]`). Extra columns are ignored.

For a row type of plain fields (numbers, bools, strings, `T | null` of those, `u8[]`) and a
source class whose methods don't throw, the compiler reads the rows column by column from the
source itself: each column is matched to its field once, and no text is made. Any other row
type (nested objects, arrays, unions) is read through the JSON text of the rows, with the same
result.

```ts
import { CellKind, readRows, RowSource } from "velt/db";

// Rows of `key=value` pairs, one row per line.
class Pairs implements RowSource {
  lines: string[];
  row: i64 = -1;

  constructor(text: string) {
    this.lines = text.split("\n");
  }
  columnCount(): i64 {
    return 2;
  }
  columnName(i: i64): string {
    return i == 0 ? "key" : "value";
  }
  next(): bool {
    this.row++;
    return this.row < (this.lines.length as i64);
  }
  kind(i: i64): CellKind {
    return i == 0 ? CellKind.Text : CellKind.Int;
  }
  bool(i: i64): bool {
    return false;
  }
  int(i: i64): i64 {
    return parseInt(this.lines[this.row].split("=")[1], 10) as i64;
  }
  float(i: i64): f64 {
    return 0;
  }
  text(i: i64): string {
    return this.lines[this.row].split("=")[0];
  }
  blob(i: i64): u8[] {
    return [];
  }
  json(i: i64): string {
    return "null";
  }
}

struct Pair {
  key: string;
  value: i64;
}

function main() {
  for (const p of readRows<Pair, Pairs>(new Pairs("a=1\nb=2"))) {
    console.log(p.key, p.value); // a 1, then b 2
  }
}
```

## Native drivers

A driver in a package with a Rust library ([Packages with native code](../book/native-packages.md))
hands its rows and parameters across without JSON text in between, through two objects its
library owns: a row cursor over its live result and a parameter sink for a statement
([native_abi.md "Row cursors" and "Parameter sinks"](../internals/contracts/native_abi.md); crate
`velt_text::rows_cursor` on the Rust side). The library registers each one with the runtime and
gives its Velt side a handle (a `u64`), never an address: std looks the handle up, so a number
that names no open cursor or sink (made up, closed already, the other kind) never reaches
memory. The decoder and the binder are generated for each type by the compiler.

- `decodeRows<T>(cursor: u64): T[]`: every row of the cursor as a `T`, each column filling the
  field of its name with the same conversions and messages as `readRows`. The cursor stays the
  driver's: give it back afterwards, so the driver can release the result and report a failure it
  met while stepping. Throws `RowError` for a row that doesn't fit, and for a handle that names
  no open row cursor.
- `decodeRow<T>(cursor: u64): T`: the row of a cursor the driver opened for a single row.
- `bindParams<P>(sink: u64, params: P): bool`: hands `params` to the sink value by value: an
  array, a tuple or a lone value by position, an object's fields by name, each value as its JSON
  text would read back (an integral number an integer, NaN null, a `u8[]` field a blob). Returns
  false, having done nothing, for a type it can't take that way (a nested object, a union): the
  driver then takes `JSON.stringify(params)`, which binds the same values. False too for a
  handle that names no open parameter sink.

```ts ignore
import { bindParams, decodeRows } from "velt/db";

declare function velt_mydb__bind_begin(stmt: u64): u64;
declare function velt_mydb__query_bound(sink: u64): u64;
declare function velt_mydb__query_json(stmt: u64, params: string): u64;
declare function velt_mydb__close_rows(cursor: u64): void;

export function query<T, P>(stmt: u64, params: P): T[] {
  const sink = velt_mydb__bind_begin(stmt);
  const cursor = bindParams<P>(sink, params)
    ? velt_mydb__query_bound(sink)
    : velt_mydb__query_json(stmt, JSON.stringify(params));
  try {
    return decodeRows<T>(cursor);
  } finally {
    velt_mydb__close_rows(cursor);
  }
}
```

`packages/sqlite` and `packages/postgres` in the Velt repository are complete drivers built this
way.
