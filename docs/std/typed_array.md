# Typed arrays, ArrayBuffer and DataView

`ArrayBuffer`, `DataView` and the typed arrays `Uint8Array`, `Int8Array`, `Uint8ClampedArray`,
`Int16Array`, `Uint16Array`, `Int32Array`, `Uint32Array`, `Float32Array` and `Float64Array` are
globals, as in Node (no import; the module is `velt:typed_array`). They behave as in Node 22:
the same conversions, errors and `console.log` / `JSON.stringify` output.

```ts
function main() {
  const bytes = new Uint8Array([1, 2, 300]);       // 300 is stored as 44, as in JS
  bytes[0] += 10;
  console.log(bytes);                               // Uint8Array(3) [ 11, 2, 44 ]
  const view = bytes.subarray(1);                   // shares the bytes
  view[0] = 9;
  console.log(bytes[1], bytes.slice(1, 2));         // 9 Uint8Array(1) [ 9 ]
  const f = new Float64Array([0.5]);
  console.log(new Uint8Array(f.buffer));            // the 8 bytes of 0.5, little-endian
  const dv = new DataView(new ArrayBuffer(4));
  dv.setUint16(0, 0x1234);                          // big-endian unless littleEndian is true
  console.log(dv.getUint8(0), dv.getUint16(0, true)); // 18 13330
  console.log(JSON.stringify(bytes));               // {"0":11,"1":9,"2":44}
}
```

- **A typed array is a view** of a byte range of an `ArrayBuffer`: `new Uint8Array(n)` (`n`
  zeros in a new buffer), `new Uint8Array([1, 2])` or `new Uint8Array(otherTypedArray)` (a copy,
  converted), and `new Uint8Array(buffer, byteOffset, length)` (a view of `buffer`). Views of
  one buffer see each other's writes. `buffer`, `byteOffset`, `byteLength`, `length` and
  `BYTES_PER_ELEMENT` are as in JS; multi-byte elements are little-endian (the platform's order,
  little-endian on every target Velt has).
- **Elements are numbers.** `xs[i]` reads one; a write converts the number as JS does
  (`ToUint8`: 300 is 44 and -1 is 255; `Uint8ClampedArray` clamps and rounds halves to even;
  `Float32Array` rounds to the nearest `f32`). A write outside the array does nothing, as in JS.
  A **read** outside it panics like an array index (`index out of bounds: the len is 2 but the
  index is 5`), where JS reads `undefined`; `xs.at(i)` returns `undefined` there, as in JS. `find` and `findLast` give `undefined` when
  nothing matches.
- **Methods**: `at`, `copyWithin`, `entries`, `every`, `fill`, `filter`, `find`, `findIndex`,
  `findLast`, `findLastIndex`, `forEach`, `includes`, `indexOf`, `join`, `keys`,
  `lastIndexOf`, `map`, `reduce`, `reduceRight`, `reverse`, `set`, `slice`, `some`, `sort`,
  `subarray`, `toLocaleString`, `toReversed`, `toSorted`, `toString`, `values`, `with`,
  iteration (`for...of`, `[...xs]`), and the statics `from` and `of`. `subarray` shares the
  buffer, `slice` copies. `sort()` is numeric (`-0` before `0`, NaN last), as for typed arrays
  in JS. As for Velt's arrays, `entries`, `keys` and `values` return arrays rather than
  iterators, `reduce` takes its initial value, and callbacks get the element and its index.
- **`ArrayBuffer`**: `new ArrayBuffer(byteLength)` (zeros), `byteLength`, `slice(begin, end)`
  (a copy) and `ArrayBuffer.isView(v)`. It is not resizable (`resizable` is `false`) and has no
  `transfer`.
- **`DataView`**: `new DataView(buffer, byteOffset, byteLength)` and `getInt8` …
  `getFloat64` / `setInt8` … `setFloat64` at any byte offset, big-endian unless the
  `littleEndian` argument is `true`. An access outside the view throws `RangeError`
  (`Offset is outside the bounds of the DataView`).
- **Errors** are `RangeError`s with V8's messages: `Invalid typed array length: -1`,
  `start offset of Uint16Array should be a multiple of 2`, `offset is out of bounds` (`set`),
  `Invalid array buffer length`, ….
- **Bytes from and to the rest of std**: `new TextEncoder().encode(s)` returns a `Uint8Array`
  (the string's bytes, not copied again), `encodeInto(s, dest)` writes into one, and
  `TextDecoder.decode` takes a typed array, an `ArrayBuffer`, a `DataView` or a `u8[]`.
  `fetch` and `Response` / `Request` take them as a body, and `res.bytes()` /
  `res.arrayBuffer()` return a `Uint8Array` / an `ArrayBuffer`. Velt's own modules
  (`velt:crypto`, `velt:encoding`'s Base64 and hex, `Buffer.alloc`) work on `u8[]`, Velt's byte
  array: `Uint8Array.fromBytes(b)` makes a `Uint8Array` of a copy of a `u8[]` (so the buffer
  keeps its length whatever happens to `b` afterwards), and `xs.toBytes()` copies a
  `Uint8Array`'s elements into a `u8[]`. An index may be an integer of any type (`xs[k]` with
  `k: usize`), as for arrays.
- **Across tasks**: a typed array (or buffer) sent to `spawn` or over a channel is copied, with
  its buffer, as any value a task receives is ([Async](../reference/async.md)): the two tasks
  don't share its bytes. Views of one buffer stay views of one buffer within a task.

```ts
import { sha256 } from "velt:crypto";
import { hexEncode } from "velt:encoding";

function main() {
  const data = new TextEncoder().encode("abc");
  console.log(hexEncode(sha256(data.toBytes())).slice(0, 8)); // ba7816bf
  const digest = Uint8Array.fromBytes(sha256(data.toBytes()));
  console.log(digest.length, digest[0]);                      // 32 186
}
```

Not supported yet: `BigInt64Array` and `BigUint64Array` (and `DataView`'s `getBigInt64` /
`setBigInt64`), which wait for `bigint` values (#1088); `SharedArrayBuffer`, resizable buffers,
`Float16Array`; `ArrayBuffer.isView` of a value of any type (it takes an `ArrayBuffer`, a typed
array, a `DataView` or `null` until `unknown` arrives, #924); constructing a typed array from an
iterable that is not an array or a typed array (use `Uint8Array.from(set)`). A `Float64Array`
holding Velt's `NaN` has the platform's NaN bytes (`0x7ff8…` on arm64, `0xfff8…` on x86-64),
where V8's are `0x7ff8…`.
