# velt:stdin

`import { readLine } from "velt:stdin"`. Reads standard input. The sync and async readers share
one buffered stream. Text is UTF-8, and invalid bytes become U+FFFD.

- `readLine(): Promise<string | null>` / `readLineSync()`: the line without `\n` / `\r\n`, or
  null at end of input.
- `readAll(): Promise<string>` / `readAllSync()`.

```ts
import { readLine } from "velt:stdin";

async function main() {
  let n = 0;
  while (true) {
    const line = await readLine();
    if (line == null) {
      break;
    }
    n++;
    console.log(`${n}: ${line}`);
  }
}
```
