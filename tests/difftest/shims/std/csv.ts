// Node twin of std/csv for the differential harness. Node has no CSV parser, so this is an
// independent character-level state machine written from RFC 4180 and the std/csv docs (not a
// port of its byte scanner): records end at LF, CRLF or a lone CR, a final line break starts no
// record, a leading BOM is skipped, `trim` strips spaces/tabs around fields.

export class CsvError extends Error {
  line: number;
  constructor(message: string, line: number) {
    super(message);
    this.line = line;
  }
}

type ReadOpts = { delimiter?: string; trim?: boolean; skipEmptyLines?: boolean };
type WriteOpts = { delimiter?: string; crlf?: boolean };

function delimiterOf(d: string): string {
  const c = d.charCodeAt(0);
  if (d.length !== 1 || c >= 128 || d === '"' || d === "\n" || d === "\r") {
    throw new CsvError(`Invalid CSV delimiter: ${JSON.stringify(d)}`, 0);
  }
  return d;
}

const isBlank = (c: string): boolean => c === " " || c === "\t";

export function parseCsv(text: string, opts: ReadOpts = {}): string[][] {
  const delim = delimiterOf(opts.delimiter ?? ",");
  const trim = opts.trim ?? false;
  const s = text.startsWith("﻿") ? text.slice(1) : text;
  const rows: string[][] = [];
  let row: string[] = [];
  let line = 1;
  let i = 0;
  const endRecord = () => {
    const blank = row.length === 1 && row[0] === "";
    if (!(opts.skipEmptyLines && blank)) rows.push(row);
    row = [];
  };
  while (i < s.length) {
    let j = i;
    if (trim) while (j < s.length && isBlank(s[j])) j++;
    let field = "";
    if (s[j] === '"') {
      const startLine = line;
      j++;
      for (;;) {
        if (j >= s.length) {
          throw new CsvError(`Unterminated quoted field starting on line ${startLine}`, startLine);
        }
        if (s[j] === '"' && s[j + 1] === '"') {
          field += '"';
          j += 2;
        } else if (s[j] === '"') {
          j++;
          break;
        } else {
          if (s[j] === "\n" || (s[j] === "\r" && s[j + 1] !== "\n")) line++;
          field += s[j++];
        }
      }
      if (trim) while (j < s.length && isBlank(s[j])) j++;
      if (j < s.length && s[j] !== delim && s[j] !== "\n" && s[j] !== "\r") {
        throw new CsvError(`Unexpected character after closing quote on line ${line}`, line);
      }
    } else {
      while (j < s.length && s[j] !== delim && s[j] !== "\n" && s[j] !== "\r") field += s[j++];
      if (trim) field = field.replace(/[ \t]+$/, "");
    }
    row.push(field);
    if (j >= s.length) {
      endRecord();
      break;
    }
    const c = s[j++];
    if (c !== delim) {
      if (c === "\r" && s[j] === "\n") j++;
      line++;
      endRecord();
    } else if (j >= s.length) {
      row.push("");
      endRecord();
    }
    i = j;
  }
  return rows;
}

export function parseCsvRecords(text: string, opts: ReadOpts = {}): Map<string, string>[] {
  const rows = parseCsv(text, opts);
  return rows.slice(1).map((r) => new Map(rows[0].map((h, c) => [h, r[c] ?? ""])));
}

export function stringifyCsv(rows: string[][], opts: WriteOpts = {}): string {
  const delim = delimiterOf(opts.delimiter ?? ",");
  const eol = opts.crlf ? "\r\n" : "\n";
  const quote = (f: string) => (/["\n\r]/.test(f) || f.includes(delim) ? `"${f.replaceAll('"', '""')}"` : f);
  return rows.map((r) => r.map(quote).join(delim) + eol).join("");
}
