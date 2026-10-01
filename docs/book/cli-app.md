# Building a command-line tool

This guide builds `count`, a small `wc`-like tool: arguments and `--help`, reading files,
error messages and exit codes, and tests. A Velt program is a single native executable with no
runtime to install and no warm-up, which suits command-line tools.

`velt new tool --template cli` generates a similar skeleton with subcommands and tests.

## The program

```ts
import { ArgParser, CliError } from "velt:cli";
import { readFileSync } from "velt:fs";
import { args } from "velt:process";

class InputError extends Error {}

function parser(): ArgParser {
  return new ArgParser("count", "Counts lines, words and bytes.")
    .flag("lines", { short: "l", help: "Print only the line count" })
    .option("top", { short: "t", default: "0", help: "Also print the n most common words", valueName: "n" })
    .positional("file", "The file to read");
}

function read(file: string): string {
  try {
    return readFileSync(file);
  } catch (e) {                                   // e: IoError
    throw new InputError(`cannot read ${file} (${e.code})`);
  }
}

function words(text: string): string[] {
  return text.replaceAll("\n", " ").split(" ").filter((w) => w !== "");
}

function topWords(text: string, n: i64): string[] {
  const freq = new Map<string, i64>();
  for (const w of words(text)) {
    freq.upsert(w.toLowerCase(), 1, (c) => c + 1);
  }
  const entries = freq.entries();
  entries.sort((a, b) => b[1] - a[1]);
  return entries.slice(0, n).map((e) => `${e[1]} ${e[0]}`);
}

// Runs the command line `argv` and returns the lines to print.
export function run(argv: string[]): string[] {
  const p = parser();
  const opts = p.parse(argv);
  if (opts.flag("help")) {
    return [p.help()];
  }
  const file = opts.get("file");
  if (file == null) {
    throw new CliError("missing <file> (try --help)");
  }
  const text = read(file);
  const lines = text.split("\n").length - 1;
  if (opts.flag("lines")) {
    return [`${lines}`];
  }
  const out = [`${lines} ${words(text).length} ${text.length} ${file}`];
  return out.concat(topWords(text, opts.getInt("top") ?? 0));
}

function main(): i32 {
  try {
    for (const line of run(args())) {
      console.log(line);
    }
    return 0;
  } catch (e) {                                   // e: CliError | InputError
    console.error(`count: ${e.message}`);
    return e instanceof CliError ? 2 : 1;
  }
}
```

```sh
$ velt build --release count.vlt
$ ./target/velt/count poem.txt --top 2
2 7 28 poem.txt
3 the
1 cat
$ ./target/velt/count --nope
count: Unknown option '--nope'
$ ./target/velt/count missing.txt
count: cannot read missing.txt (ENOENT)
$ ./target/velt/count --help
Usage: count [options] [file]

Counts lines, words and bytes.

Arguments:
  file           The file to read

Options:
  -h, --help     Show this help
  -l, --lines    Print only the line count
  -t, --top <n>  Also print the n most common words (default: 0)
```

## How it fits together

- **Arguments**: [`velt:cli`](../std/cli.md)'s `ArgParser` follows Node's `util.parseArgs`
  rules (`--x`, `--x=v`, `-abc`, `--no-x`, `--`). Builder calls chain; `parse` throws a
  `CliError` for an unknown option or a missing value, and `help()` renders the usage text.
  `parse` takes the argument array, so the logic is testable without a process.
- **Exit codes**: `main` returns an `i32`, the process's exit code. Here usage errors exit
  with 2 and input errors with 1. `exit(code)` from [`velt:process`](../std/process.md) exits
  immediately from anywhere.
- **Typed errors**: `read` converts the `IoError` from `readFileSync` (whose `code` is a
  Node-style name such as `ENOENT`) into an `InputError`, so `main`'s `catch` sees exactly
  `CliError | InputError` and can choose the exit code with `instanceof`.
- **Output**: `console.log` goes to stdout, `console.error` to stderr. For large outputs,
  build the text first and print it once, or write in chunks with
  [`velt:fs_stream`](../std/fs_stream.md).
- **Input**: `readFileSync` reads a whole file; [`velt:stdin`](../std/stdin.md) reads standard
  input line by line or at once; [`velt:fs_stream`](../std/fs_stream.md) reads large files in
  chunks.

## Testing

Because `run` takes the argument array and returns lines, tests call it directly. In
`tests/count.test.vlt`:

```ts ignore
import { run } from "../src/main";

export function test_counts() {
  assertEq(run(["tests/data/poem.txt", "-l"]), ["2"]);
}

export function test_usage_error() {
  try {
    run([]);
    assert(false, "expected a CliError");
  } catch (e) {
    assertEq(e.message, "missing <file> (try --help)");
  }
}
```

`velt test` runs every exported `test_*` function ([Testing](testing.md)).

## Running other programs

[`velt:child_process`](../std/child_process.md) runs other programs and captures their output:

```ts
import { execSync } from "velt:child_process";

function main() {
  const r = execSync("git", ["rev-parse", "--short", "HEAD"]);
  console.log(r.ok ? r.stdout.trim() : `git failed (${r.code})`);
}
```

## Shipping it

`velt build --release` produces one executable with no runtime dependency beyond the system C
library. Build on the oldest Linux distribution you target (glibc is linked dynamically). See
[Platforms](../tooling/platforms.md).
