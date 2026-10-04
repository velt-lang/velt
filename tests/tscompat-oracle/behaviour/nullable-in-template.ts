// `Map.get` of a missing key prints `null` in Velt and `undefined` in JavaScript.

export function main() {
  const m = new Map<string, string>();
  console.log(`value: ${m.get("missing")}`);
}
