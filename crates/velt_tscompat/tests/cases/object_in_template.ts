// A template literal formats objects like `console.log` in Velt; JavaScript calls `toString()`.
// Primitives, enums, classes that declare `toString()` and arrays of primitives print the same.

export type Point = { x: number; y: number };

export enum Level {
  Low,
  High,
}

export class Money {
  cents: number = 0;
  toString(): string {
    return `${this.cents / 100}`;
  }
}

export class Euro extends Money {}

export function describe(p: Point, xs: number[], m: Money, e: Euro, l: Level): string {
  const a = `${p}`; //~ object-in-template
  const b = `${xs} ${[xs]}`;
  const c = `${m} ${l} ${p.x} ${xs.length}`;
  const d = `${e}`; //~ object-in-template
  return a + b + c + d;
}

export function pair(t: [number, string], ns: (number | null)[], ps: Point[], ms: Money[]): string {
  const a = `${ps}`; //~ object-in-template
  const b = `${ms}`; //~ object-in-template
  return `${t} ${ns}` + a + b;
}
