// A value that may be `null` prints `null` in Velt; in JavaScript it may be `undefined`.

export type User = { name: string; nickname?: string };

export function label(u: User, scores: Map<string, number>): string {
  const a = `${u.nickname}`; //~ nullable-in-template
  const b = `${scores.get(u.name)}`; //~ nullable-in-template
  const c = `${u.nickname ?? u.name}`;
  const v = scores.get(u.name);
  const d = `${v}`; //~ nullable-in-template
  return a + b + c + d;
}

// A value that is never `undefined` prints `null` in both languages.
export type Item = { label: string | null };

export function show(i: Item, n: number | null): string {
  return `${i.label} ${n}`;
}
