// A template literal formats objects and arrays like `console.log` in Velt; JavaScript calls
// `toString()`. Primitives, enums and classes that declare `toString()` print the same.

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
  const b = `${xs}`; //~ object-in-template
  const c = `${m} ${l} ${p.x} ${xs.length}`;
  const d = `${e}`; //~ object-in-template
  return a + b + c + d;
}

export function pair(t: [number, string]): string {
  return `${t}`; //~ object-in-template
}
