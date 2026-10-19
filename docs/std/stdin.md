# velt:stdin

`import { lines, readLine } from "velt:stdin"`. Reads standard input. The sync and async readers
share one buffered stream. Text is UTF-8, and invalid bytes become U+FFFD.

- `readLine(): Promise<string | null>` / `readLineSync()`: the line without `\n` / `\r\n`, or
  null at end of input.
- `lines(): AsyncGenerator<string, IoError>`: the remaining lines, as `readLine` reads them, for
  `for await (const line of lines())` (in Node: `for await (const line of
  readline.createInterface({ input: process.stdin }))`).
- `readAll(): Promise<string>` / `readAllSync()`: everything left on standard input.
- `readAllBytes(): Promise<u8[]>` / `readAllBytesSync()`: the same, as raw bytes.

```ts
import { lines } from "velt:stdin";

async function main() {
  let n = 0;
  for await (const line of lines()) {
    n++;
    console.log(`${n}: ${line}`);
  }
}
```
