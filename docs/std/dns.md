# velt:dns

`import { lookup } from "velt:dns"`. Host name resolution through the system resolver.

- `lookup(host): Promise<string[]>`: every address, IPv4 and IPv6, without duplicates. Throws
  `IoError` `ENOENT` when the name doesn't resolve.
- `lookupOne(host): Promise<string>`: the address a connect would try first.

```ts
import { lookup, lookupOne } from "velt:dns";

async function main() {
  console.log((await lookup("localhost")).length > 0, await lookupOne("127.0.0.1"));
  try {
    await lookup("no-such-host.invalid");
  } catch (e) {
    console.log(e.code); // ENOENT
  }
}
```
