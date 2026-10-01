# velt:cli

`import { ArgParser } from "velt:cli"`. Argument parsing with Node `util.parseArgs` rules and a
commander-style builder. `parse` takes the argv array, so it is easy to test. Pass it `args()`
from `velt:process`.

- `new ArgParser(program, description = "")`: every parser has `-h, --help`, which you can
  redeclare.
- Builder methods consume the parser and return it, so chain them:
  - `.flag(name, { short?, help? })`
  - `.option(name, { short?, default?, help?, valueName? })`
  - `.positional(name, help = "")`
- `parse(args): ParsedArgs`: throws `CliError` for an unknown option, a missing value, or a
  value given to a flag. `help(): string` returns aligned usage text.
- Accepted syntax: `--x`, `--x=v`, `--x v`, `--no-x`, `-abc`, `-o v`, `-ov`, `--` and `-`.
- `ParsedArgs`:
  - `positionals`, `flag(name)`
  - `get(name): string | null`: the last value, else the default, else the named positional
  - `getOr(name, d)`, `getAll(name)`, `has(name)`
  - `getInt(name): i64 | null`: throws `CliError` if the value is not an integer

```ts
import { ArgParser } from "velt:cli";
import { args } from "velt:process";

function main(): i32 {
  const p = new ArgParser("resize", "Resizes images.")
    .flag("verbose", { short: "v", help: "Print progress" })
    .option("width", { short: "w", default: "800", help: "Target width", valueName: "px" })
    .positional("input", "Image to resize");
  try {
    const a = p.parse(args());
    if (a.flag("help")) {
      console.log(p.help());
      return 0;
    }
    console.log(a.flag("verbose"), a.getInt("width"), a.get("input"));
  } catch (e) {
    console.error(e.message);
    return 2;
  }
  return 0;
}
```

Notes: positionals are optional and extra ones are allowed; check `positionals.length`. An
option's value is taken verbatim even if it starts with `-`.
