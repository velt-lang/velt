// An operator method (`[Symbol.add](other)`, which `a + b` calls in Velt); `tsc` knows no
// `Symbol.add`, and `+` on objects is a type error there.

export class Vec {
  constructor(public x: number) {}

  [Symbol.add](other: Vec): Vec {
    return new Vec(this.x + other.x);
  }
}

// An index with several keys (`g[0, 1]`, an operator indexer in Velt) is the comma operator in
// TypeScript, and `tsc` rejects a left operand without side effects (TS2695).
export class Grid {
  [Symbol.index](i: number, j: number): number {
    return i + j;
  }
}

export function corner(g: Grid): number {
  return g[0, 1];
}
