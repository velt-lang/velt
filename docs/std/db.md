# velt/db

`import { CellKind, readRow, readRows, RowSource, RowError } from "velt/db"`. What database
drivers share: typed rows from any source of tabular data. A driver written in Velt (a database's wire protocol parsed in
place, a file format) implements `RowSource`, and `readRows<T>` fills one `T` per row: each
column fills the field of its name, as `JSON.parse<T>` fills fields from keys, with the same
conversions (an integer into an `f64` field, an exact float into an integer field, `null` into a
`T | null` field) and the same messages for a row that doesn't fit. velt/sqlite and
velt/postgres read their rows the same way.

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

Native drivers (written in Rust on the native SDK) offer the same through a row cursor
([native_abi.md "Row cursors"](../internals/contracts/native_abi.md)).
