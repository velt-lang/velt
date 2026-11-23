// The Node twin of tests/golden/std/bytes_utf8_slice.velt: Node's `buf.utf8Slice` clamps no
// positions and throws on bad ones, so the twin uses `toString("utf8", start, end)`, which
// clamps them as Velt's `utf8Slice` does (a negative start is 0).
function show(s) {
  return `${s.length} ${JSON.stringify(s)}`;
}

function slice(b, start = 0, end = b.length) {
  return Buffer.from(b).toString("utf8", start, end);
}

const ascii = Buffer.from("abcdefghijklmnopqrstuvwxyz0123456789");
console.log(show(slice(ascii, 0, 22)));
console.log(show(slice(ascii, 0, 23)));
console.log(show(slice(ascii, 0, 24)));
console.log(show(slice(ascii, 5, 36)));
console.log(show(slice(ascii, 30, 99)));
console.log(show(slice(ascii, -3, 4)));
console.log(show(slice(ascii, 10, 5)));
const long = [];
for (let i = 0; i < 300; i++) long.push(97 + (i % 26));
console.log(show(slice(long, 1, 299)));
const mixed = Buffer.from("añ€𝄞 and ascii after the non-ascii start, long enough");
console.log(show(slice(mixed)));
console.log(show(slice(mixed, 1, 3)));
console.log(show(slice(mixed, 1, 2)));
console.log(show(slice(mixed, 3, 6)));
console.log(show(slice(mixed, 6, 9)));
console.log(show(slice(mixed, 6, 10)));
const bad = Buffer.from([0x61, 0x80, 0x62, 0xc0, 0xaf, 0x63, 0xed, 0xa0, 0x80, 0x64, 0xf0, 0x9f]);
console.log(show(slice(bad)));
console.log(show(slice(bad, 0, 3)));
console.log(show(slice(bad, 10, 12)));
console.log(show(slice(bad, 2, 6)));
const longBad = [];
for (let i = 0; i < 40; i++) longBad.push(0x78);
longBad.push(0xff, 0x79);
console.log(show(slice(longBad)));
