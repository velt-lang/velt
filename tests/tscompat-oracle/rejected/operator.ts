// An operator method (`[Symbol.add](other)`, which `a + b` calls in Velt); `tsc` knows no
// `Symbol.add`, and `+` on objects is a type error there.

export class Vec {
  constructor(public x: number) {}

  [Symbol.add](other: Vec): Vec {
    return new Vec(this.x + other.x);
  }
}
