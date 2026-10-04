// `Map.get` of a missing key is `null` in Velt, `undefined` in JavaScript, so `=== null` is
// true in Velt and false in JavaScript. The fix tests with `==`, which matches both.

export function main() {
  const m = new Map<string, number>();
  const found = m.get("missing");
  console.log(found === null ? "missing" : "found");
}
