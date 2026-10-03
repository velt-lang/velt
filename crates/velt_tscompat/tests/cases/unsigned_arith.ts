// A length is unsigned in Velt: below zero it wraps around, where JavaScript goes negative.

export function lastIndex(xs: string[]): number {
  return xs.length - 1; //~ unsigned-arith
}

export function before(xs: string[], i: number): number {
  return i - 1;
}
