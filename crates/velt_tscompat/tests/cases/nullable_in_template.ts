// A value that may be `null` prints `null` in Velt; in JavaScript it may be `undefined`.

export type User = { name: string; nickname?: string };

export function label(u: User, scores: Map<string, number>): string {
  const a = `${u.nickname}`; //~ nullable-in-template
  const b = `${scores.get(u.name)}`; //~ nullable-in-template
  const c = `${u.nickname ?? u.name}`;
  return a + b + c;
}
