#!/usr/bin/env python3
"""Generate a feature-style Velt application of about N lines (build-speed issue #2707's shape):
one service of features, each a record type, a parser with error paths, JSON in and out, a
registry over a `Map`, an interface with implementations (subcommands), and a router that
dispatches to them. `main` runs every feature and prints the sum, so nothing is dead code.

    gen.py <lines> <out dir>

Writes <out dir>/main.velt and one module per feature (about 120 lines each).
"""
import os
import sys

FEATURE = r"""export type Row@K@ = { id: number; name: string; price: number; tags: string[] };

export class Record@K@ {
  id: number;
  name: string;
  price: number;
  tags: string[];
  constructor(id: number, name: string, price: number, tags: string[]) {
    this.id = id;
    this.name = name;
    this.price = price;
    this.tags = tags;
  }
  label(): string {
    return `${this.name}#${this.id} (${this.tags.join(",")})`;
  }
}

export class ParseError@K@ extends Error {
  line: number;
  constructor(line: number, message: string) {
    super(message);
    this.line = line;
  }
}

export function parse@K@(text: string): Record@K@[] {
  const out: Record@K@[] = [];
  const lines = text.split("\n");
  for (let i = 0; i < lines.length; i++) {
    const cols = lines[i].split(";");
    if (cols.length < 3 || !/^[0-9]+$/.test(cols[0])) {
      throw new ParseError@K@(i + 1, `bad row: ${lines[i]}`);
    }
    const tags = cols.length > 3 ? cols[3].split(",") : [];
    out.push(new Record@K@(Number(cols[0]), cols[1], Number(cols[2]), tags));
  }
  return out;
}

export function print@K@(rs: Record@K@[]): string {
  return rs.map((r) => r.label()).join("\n");
}

export function toJson@K@(r: Record@K@): string {
  const row: Row@K@ = { id: r.id, name: r.name, price: r.price, tags: r.tags };
  return JSON.stringify(row);
}

export function fromJson@K@(text: string): Record@K@ {
  const row = JSON.parse<Row@K@>(text);
  return new Record@K@(row.id, row.name, row.price, row.tags);
}

export class Registry@K@ {
  items: Map<string, Record@K@> = new Map();
  add(r: Record@K@): void {
    this.items.set(r.name, r);
  }
  get(name: string): Record@K@ | undefined {
    return this.items.get(name);
  }
  total(): number {
    let t = 0;
    for (const r of this.items.values()) t += r.price;
    return t;
  }
}

export interface Command@K@ {
  name: string;
  run(reg: Registry@K@, args: string[]): number;
  help(): string;
}

export class ListCommand@K@ implements Command@K@ {
  name = "list@K@";
  run(reg: Registry@K@, args: string[]): number {
    return reg.items.size + args.length;
  }
  help(): string {
    return "list the records";
  }
}

export class ShowCommand@K@ implements Command@K@ {
  name = "show@K@";
  run(reg: Registry@K@, args: string[]): number {
    const r = reg.get(args[0] ?? "");
    return r === undefined ? 0 : r.label().length;
  }
  help(): string {
    return "show one record";
  }
}

export class TotalCommand@K@ implements Command@K@ {
  name = "total@K@";
  seen: string[] = [];
  run(reg: Registry@K@, args: string[]): number {
    this.seen.push(args.join(" "));
    return reg.total();
  }
  help(): string {
    return "sum the prices";
  }
}

export function route@K@(cmds: Command@K@[], reg: Registry@K@, name: string, args: string[]): number {
  for (const c of cmds) {
    if (c.name === name) return c.run(reg, args);
  }
  return -1;
}

export function feature@K@(): number {
  const reg = new Registry@K@();
  let n = 0;
  try {
    for (const r of parse@K@("1;apple;3;red,fresh\n2;pear;4\n3;plum;5;blue")) reg.add(r);
    parse@K@("x;bad");
  } catch (e) {
    if (e instanceof ParseError@K@) n += e.line;
  }
  const back = fromJson@K@(toJson@K@(reg.get("pear")!));
  n += back.price + print@K@([back]).length;
  const cmds: Command@K@[] = [new ListCommand@K@(), new ShowCommand@K@(), new TotalCommand@K@()];
  n += route@K@(cmds, reg, "list@K@", ["a"]);
  n += route@K@(cmds, reg, "show@K@", ["plum"]);
  n += route@K@(cmds, reg, "total@K@", []);
  return n;
}
"""


def main() -> None:
    lines = int(sys.argv[1])
    out = sys.argv[2]
    os.makedirs(out, exist_ok=True)
    per = FEATURE.count("\n")
    n = max(1, lines // per)
    imports, calls = [], []
    for k in range(n):
        with open(os.path.join(out, f"feature{k}.velt"), "w") as f:
            f.write(FEATURE.replace("@K@", str(k)))
        imports.append(f'import {{ feature{k} }} from "./feature{k}";')
        calls.append(f"  n += feature{k}();")
    with open(os.path.join(out, "main.velt"), "w") as f:
        f.write("\n".join(imports) + "\n\nlet n = 0;\n" + "\n".join(calls) + "\nconsole.log(n);\n")
    print(f"{n} features, {n * per} lines")


main()
