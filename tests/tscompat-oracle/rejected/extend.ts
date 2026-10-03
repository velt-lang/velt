// `extend` adds methods to a type declared elsewhere; `tsc` has no such declaration.

export type Money = { cents: number };

extend Money {
  dollars(): number {
    return this.cents / 100;
  }
}
