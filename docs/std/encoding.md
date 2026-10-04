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
