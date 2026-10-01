# velt:uuid

`import { uuidv4, uuidv7 } from "velt:uuid"`. RFC 9562 UUIDs. The text form is lowercase
`8-4-4-4-12`; the binary form is 16 bytes in network order.

- `uuidv4()`: random. `uuidv7()`: starts with 48 bits of Unix milliseconds, so ids sort by
  creation time; ids created in the same millisecond are in random order.
- `uuidParse(s): u8[]` (either case), `uuidStringify(bytes)`: both throw `UuidError`.
- `uuidValidate(s): bool`: true for a known version and variant, or the nil/max UUID.
  `uuidVersion(s): i64`. `NIL_UUID`.

```ts
import { uuidv4, uuidv7, uuidParse, uuidStringify, uuidValidate, uuidVersion } from "velt:uuid";

function main() {
  const id = uuidv7();
  console.log(id.length, uuidVersion(id), uuidVersion(uuidv4()), uuidValidate(id)); // 36 7 4 true
  const bytes = uuidParse("6BA7B810-9DAD-11D1-80B4-00C04FD430C8");
  console.log(bytes.length, uuidStringify(bytes));
}
```
