# velt:csv

`import { parseCsv, stringifyCsv } from "velt:csv"`. RFC 4180 CSV. Quoted fields may contain
delimiters, `""` escapes and line breaks. Records end at LF, CRLF or a lone CR. A final line
break doesn't add a record, and a BOM is skipped.

- `parseCsv(text, opts: CsvOptions = {}): string[][]`, where `CsvOptions { delimiter?; trim?;
  skipEmptyLines? }`.
- `parseCsvRecords(text, opts): Map<string, string>[]`: the first row is the header. Missing
  fields become `""` and extra fields are ignored.
- `stringifyCsv(rows, opts: CsvWriteOptions { delimiter?; crlf? } = {})`: quotes only when
  needed, and ends every record with a line break.
- `CsvError { message; line }`: thrown for an unterminated quote, text after a closing quote, or
  a bad delimiter (`line` is 0 for the delimiter case).

```ts
import { parseCsv, parseCsvRecords, stringifyCsv } from "velt:csv";

function main() {
  const text = "name,city\nAda,London\n\"Hopper, Grace\",\"New\nYork\"\n";
  console.log(JSON.stringify(parseCsv(text)));
  for (const r of parseCsvRecords(text)) {
    console.log(r.get("name"), "|", r.get("city"));
  }
  console.log(JSON.stringify(stringifyCsv([["id", "note"], ["1", "say \"hi\""]], { crlf: true })));
  try {
    parseCsv("a\n\"open");
  } catch (e) {
    console.log(e.line, e.message); // 2 Unterminated quoted field starting on line 2
  }
}
```
