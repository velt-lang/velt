// Velt writes a `Map` as an object; JavaScript's `JSON.stringify` writes `{}`.

export function main() {
  const m = new Map<string, number>();
  m.set("a", 1);
  console.log(JSON.stringify(m));
}
