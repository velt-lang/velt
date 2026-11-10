#!/usr/bin/env python3
"""Generate the application benchmark of the build-speed yardstick (#2707): one service of N
features, about 233 lines each, written the way an application is (yardstick.py measures it).

    app.py <lines> <out dir>

Each feature is one module, `f<K>.velt`:
- a record (`interface Item<K>`) and a second record for its JSON envelope;
- a parser of comma-separated rows with error paths (an error class per feature, a regex test,
  `Number`, `split`, `push`) and a printer;
- JSON in and out (`JSON.stringify`, `JSON.parse(text) as Envelope<K>`);
- a registry over a `Map<string, Item<K>>`;
- an interface with two implementations (pricing rules);
- routes in the shared router and subcommands of the shared command line (each subcommand a
  class implementing the shared `Handler` interface);
- `run<K>`, which exercises all of it and returns a number.

`main.velt` installs every feature and calls every `run<K>` inside one `try`, as a service's
entry point does, so the errors of every feature reach it. The output is a checksum, the same
on every run. The generator is deterministic: the same arguments write the same files.
"""
import os
import sys

COMMON = """\
// The service's shared parts: a router, a command line and the interface of its subcommands.

export interface Handler {
  name(): string;
  help(): string;
  handle(args: string[]): string;
}

export class Router {
  routes: Map<string, (args: string[]) => string>;
  hits: number;

  constructor() {
    this.routes = new Map();
    this.hits = 0;
  }

  add(path: string, fn: (args: string[]) => string): void {
    this.routes.set(path, fn);
  }

  dispatch(path: string, args: string[]): string {
    const fn = this.routes.get(path);
    if (fn === undefined) {
      return `404 ${path}`;
    }
    this.hits += 1;
    return fn(args);
  }
}

export class Cli {
  commands: Map<string, Handler>;

  constructor() {
    this.commands = new Map();
  }

  command(h: Handler): void {
    this.commands.set(h.name(), h);
  }

  run(argv: string[]): string {
    if (argv.length == 0) {
      return "usage: app <command> [args]";
    }
    const h = this.commands.get(argv[0]);
    if (h === undefined) {
      return `unknown command ${argv[0]}`;
    }
    return h.handle(argv.slice(1));
  }
}
"""

FEATURE = """\
import { Handler, Router, Cli } from "./common";

export interface Item@K@ {
  id: number;
  name: string;
  qty: number;
  price: number;
  tags: string[];
}

export interface Envelope@K@ {
  kind: string;
  version: number;
  items: Item@K@[];
}

export class Item@K@Error extends Error {
  line: number;

  constructor(line: number, message: string) {
    super(`f@K@ line ${line}: ${message}`);
    this.line = line;
  }
}

export function parse@K@(text: string): Item@K@[] {
  const out: Item@K@[] = [];
  const rows = text.split("\\n");
  for (let i = 0; i < rows.length; i++) {
    const row = rows[i].trim();
    if (row.length == 0) {
      continue;
    }
    const f = row.split(",");
    if (f.length < 4) {
      throw new Item@K@Error(i + 1, `expected 4 fields, got ${f.length}`);
    }
    if (!/^[0-9]+$/.test(f[0])) {
      throw new Item@K@Error(i + 1, `bad id ${f[0]}`);
    }
    const qty = Number(f[2]);
    if (Number.isNaN(qty)) {
      throw new Item@K@Error(i + 1, `bad quantity ${f[2]}`);
    }
    out.push({ id: Number(f[0]), name: f[1], qty: qty, price: Number(f[3]), tags: f.slice(4) });
  }
  return out;
}

export function print@K@(items: Item@K@[]): string {
  const lines: string[] = [];
  for (const it of items) {
    const tags = it.tags.length > 0 ? "," + it.tags.join(",") : "";
    lines.push(`${it.id},${it.name},${it.qty},${it.price}${tags}`);
  }
  return lines.join("\\n");
}

export function toJson@K@(items: Item@K@[]): string {
  const env: Envelope@K@ = { kind: "f@K@", version: 1, items: items };
  return JSON.stringify(env);
}

export function fromJson@K@(text: string): Item@K@[] {
  const env = JSON.parse(text) as Envelope@K@;
  if (env.kind != "f@K@") {
    throw new Item@K@Error(0, `wrong kind ${env.kind}`);
  }
  return env.items;
}

export class Registry@K@ {
  items: Map<string, Item@K@>;

  constructor() {
    this.items = new Map();
  }

  add(it: Item@K@): void {
    this.items.set(it.name, it);
  }

  get(name: string): Item@K@ | undefined {
    return this.items.get(name);
  }

  remove(name: string): boolean {
    return this.items.delete(name);
  }

  list(): Item@K@[] {
    const out: Item@K@[] = [];
    for (const it of this.items.values()) {
      out.push(it);
    }
    return out;
  }

  total(rule: Rule@K@): number {
    let t = 0;
    for (const it of this.items.values()) {
      t += rule.apply(it);
    }
    return t;
  }
}

export interface Rule@K@ {
  label(): string;
  apply(it: Item@K@): number;
}

export class Plain@K@ implements Rule@K@ {
  label(): string {
    return "plain";
  }

  apply(it: Item@K@): number {
    return it.qty * it.price;
  }
}

export class Bulk@K@ implements Rule@K@ {
  limit: number;

  constructor(limit: number) {
    this.limit = limit;
  }

  label(): string {
    return `bulk>${this.limit}`;
  }

  apply(it: Item@K@): number {
    const base = it.qty * it.price;
    return it.qty > this.limit ? base * 0.9 : base;
  }
}

export class List@K@ implements Handler {
  reg: Registry@K@;

  constructor(reg: Registry@K@) {
    this.reg = reg;
  }

  name(): string {
    return "list@K@";
  }

  help(): string {
    return "list the items of f@K@";
  }

  handle(args: string[]): string {
    return print@K@(this.reg.list());
  }
}

export class Add@K@ implements Handler {
  reg: Registry@K@;

  constructor(reg: Registry@K@) {
    this.reg = reg;
  }

  name(): string {
    return "add@K@";
  }

  help(): string {
    return "add the items of the rows given";
  }

  handle(args: string[]): string {
    try {
      const items = parse@K@(args.join("\\n"));
      for (const it of items) {
        this.reg.add(it);
      }
      return `added ${items.length}`;
    } catch (e) {
      if (e instanceof Item@K@Error) {
        return `error at ${e.line}: ${e.message}`;
      }
      throw e;
    }
  }
}

export function install@K@(reg: Registry@K@, router: Router, cli: Cli): void {
  router.add("/f@K@/items", (args: string[]): string => toJson@K@(reg.list()));
  router.add("/f@K@/item", (args: string[]): string => {
    const it = reg.get(args.length > 0 ? args[0] : "");
    return it === undefined ? "not found" : `${it.id} ${it.name}`;
  });
  router.add("/f@K@/total", (args: string[]): string => `${reg.total(new Bulk@K@(3))}`);
  cli.command(new List@K@(reg));
  cli.command(new Add@K@(reg));
}

export function run@K@(router: Router, cli: Cli): number {
  const reg = new Registry@K@();
  install@K@(reg, router, cli);
  const items = parse@K@("1,a@K@,2,3.5,x,y\\n2,b@K@,4,1.25\\n3,c@K@,@K@,10");
  for (const it of items) {
    reg.add(it);
  }
  let score = 0;
  const back = fromJson@K@(router.dispatch("/f@K@/items", []));
  if (print@K@(back) != print@K@(items)) {
    score += 1000;
  }
  const rules: Rule@K@[] = [new Plain@K@(), new Bulk@K@(2)];
  for (const r of rules) {
    score += reg.total(r) + r.label().length;
  }
  score += cli.run(["add@K@", "4,d@K@,1,2", "x,bad,1,1"]).length;
  score += cli.run(["list@K@"]).length + router.dispatch("/f@K@/item", ["b@K@"]).length;
  if (reg.remove("a@K@")) {
    score += 1;
  }
  return score + Number(router.dispatch("/f@K@/total", []));
}
"""


def main():
    if len(sys.argv) != 3:
        sys.exit("usage: app.py <lines> <out dir>")
    lines, out = int(sys.argv[1]), sys.argv[2]
    feature_lines = FEATURE.count("\n")
    n = max(1, (lines - COMMON.count("\n")) // (feature_lines + 4))
    os.makedirs(out, exist_ok=True)
    total = 0

    def write(name, text):
        nonlocal total
        with open(os.path.join(out, name), "w", newline="\n") as f:
            f.write(text)
        total += text.count("\n")

    write("common.velt", COMMON)
    for k in range(n):
        write(f"f{k}.velt", FEATURE.replace("@K@", str(k)))
    main_src = ['import { Router, Cli } from "./common";\n']
    main_src += [f'import {{ run{k} }} from "./f{k}";\n' for k in range(n)]
    main_src.append("\nfunction main() {\n  const router = new Router();\n  const cli = new Cli();\n"
                    "  let sum = 0;\n  try {\n")
    main_src += [f"    sum += run{k}(router, cli);\n" for k in range(n)]
    main_src.append('  } catch (e) {\n    console.log("failed", e);\n  }\n'
                    '  console.log(`${sum} ${router.hits}`);\n}\n')
    write("main.velt", "".join(main_src))
    # The line count the yardstick divides by.
    print(f"{n} features, {total} lines")


if __name__ == "__main__":
    main()
