// `throws` clauses are Velt-only syntax.

export class Invalid extends Error {}

export function parse(s: string): number throws Invalid {
  if (s === "") {
    throw new Invalid("empty");
  }
  return 1;
}

export interface Parser {
  parse(s: string): number throws Invalid;
}

export function apply(f: (s: string) => number throws Invalid, s: string): number {
  return f(s);
}

export function check(s: string): number {
  const f = (t: string): number throws Invalid => parse(t);
  return f(s);
}
