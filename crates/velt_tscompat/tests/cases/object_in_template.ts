// A template literal writes an error and a promise as `console.log` does in Velt; JavaScript as
// `Error: message` and `[object Promise]`. Everything else prints the same in both: primitives,
// enums, arrays, and objects (through a class's `toString()`, own or inherited, else
// `[object Object]`).

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

export class Plain {
  n: number = 1;
}

export class NotFound extends Error {}

export class Shown extends Error {
  toString(): string {
    return "shown";
  }
}

export function describe(p: Point, xs: number[], m: Money, e: Euro, l: Level): string {
  const a = `${p} ${new Plain()} ${new Map<string, number>()}`;
  const b = `${xs} ${[xs]}`;
  const c = `${m} ${l} ${p.x} ${xs.length}`;
  const d = `${e}`;
  return a + b + c + d;
}

export function errors(e: Error, nf: NotFound, s: Shown, maybe: Error | null): string {
  const a = `${e}`; //~ object-in-template
  const b = `${nf}`; //~ object-in-template
  const c = `${maybe}`; //~ object-in-template
  const d = `${s} ${e.message}`;
  return a + b + c + d;
}

export async function pending(p: Promise<number>): Promise<string> {
  const a = `${p}`; //~ object-in-template
  const b = `${await p}`;
  return a + b;
}

// In an array, an object is `[object Object]` in both (an element with its own `toString()` is
// a compile error in Velt).
export function pair(t: [number, string], ns: (number | null)[], ps: Point[]): string {
  const a = `${ps}`;
  const d = `${[new Map<string, number>()]} ${[[p0()]]}`;
  return `${t} ${ns}` + a + d;
}

function p0(): Point {
  return { x: 0, y: 0 };
}
