# velt:encoding

`import { base64Encode, hexEncode } from "velt:encoding"`. Converts between bytes and text:
Base64 (RFC 4648 standard and URL-safe alphabets), lowercase hex, and UTF-8. Decoders throw
`EncodingError`.

- `base64Encode(data: u8[])` (padded), `base64Decode(s): u8[]` (padding optional).
- `base64UrlEncode(data)` (unpadded), `base64UrlDecode(s)`.
- `base64EncodeString(s)`, `base64DecodeString(s)`: Base64 of the UTF-8 bytes of a string.
- `hexEncode(data)`, `hexDecode(s)`: `hexDecode` accepts either case.
- `utf8Encode(s): u8[]` (a lone surrogate becomes U+FFFD; the length is
  `Buffer.byteLength(s)`, not `s.length`, which counts UTF-16 code units), `utf8Decode(data)`
  (strict), `utf8DecodeLossy(data)` (invalid sequences become U+FFFD), `utf8Valid(data): bool`.
- `TextEncoder` and `TextDecoder`, the WHATWG classes, are globals as in Node (no import).
  `new TextEncoder().encode(s)` gives the UTF-8 bytes as a `u8[]` (JS: a `Uint8Array`).
  `new TextDecoder(label = "utf-8", { fatal, ignoreBOM })` takes UTF-8 only: another label
  throws `EncodingError` (`The "latin1" encoding is not supported`), where Node also decodes
  legacy encodings. `decode(bytes)` drops a leading byte order mark unless `ignoreBOM`, and
  writes U+FFFD for invalid input, or throws `EncodingError` with `fatal`. There is no
  `stream` option or `encodeInto`.
- `btoa(s)` and `atob(s)`, Node's Base64 of "binary strings" (one code unit per byte), are
  globals too. `btoa` encodes a string whose code units are all at most 0xFF (Latin-1) as
  padded standard Base64; `atob` decodes as WHATWG's forgiving-base64 does: ASCII whitespace
  (tab, line feed, form feed, carriage return, space) anywhere is ignored and padding is
  optional, but at most two `=`, only at the end and only where they complete a group of four.
  The result has one code unit (0 to 255) per byte; leftover bits of the last digit are
  ignored. Bad input throws a `DOMException` named `InvalidCharacterError` (code 5) with
  Node's message: `Invalid character` (a code unit above 0xFF for `btoa`; a character outside
  the alphabet or misplaced `=` for `atob`), or `The string to be decoded is not correctly
  encoded.` (a length that cannot come from whole bytes). For bytes and UTF-8 text, use
  `base64Encode` and `base64EncodeString`. Unlike JS, the argument must be a string
  (`btoa(String(123))`).

```ts
function main() {
  const bytes = new TextEncoder().encode("héllo");
  console.log(bytes.length, new TextDecoder().decode(bytes)); // 6 héllo
}
```

```ts
function main() {
  console.log(btoa("hello"), atob(" aGVs bG8 "), atob("/w==").charCodeAt(0)); // aGVsbG8= hello 255
  try {
    atob("a");
  } catch (e) {
    console.log(e.name, e.message); // InvalidCharacterError The string to be decoded is not correctly encoded.
  }
}
```

```ts
import { base64Encode, base64DecodeString, hexEncode, hexDecode, utf8Encode, utf8Decode } from "velt:encoding";

function main() {
  const bytes = utf8Encode("héllo?");
  console.log(base64Encode(bytes), hexEncode(bytes)); // aMOpbGxvPw== 68c3a96c6c6f3f
  console.log(base64DecodeString("aMOpbGxvPw=="), utf8Decode(hexDecode("c3a9")));
  try {
    hexDecode("abc");
  } catch (e) {
    console.log(e.message); // Invalid hex: odd length
  }
}
```
