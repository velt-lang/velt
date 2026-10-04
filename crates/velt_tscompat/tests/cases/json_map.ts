// `JSON.stringify` writes a `Map` as `{}` in JavaScript, also inside other values.

export type Stock = { name: string; counts: Map<string, number> };

export function save(s: Stock, m: Map<string, number>): string {
  const a = JSON.stringify(m); //~ json-map
  const b = JSON.stringify(s); //~ json-map
  const c = JSON.stringify([m]); //~ json-map
  return a + b + c;
}

export function plain(s: { name: string; tags: string[] }): string {
  return JSON.stringify(s);
}
