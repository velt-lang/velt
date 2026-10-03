// `tsc` accepts a `declare function` without a definition; JavaScript has none, so calling it
// is a `ReferenceError`. Valid Velt only in a package with a native library.

declare function add(a: number, b: number): number;

export function sum(xs: number[]): number {
  return xs.reduce((a, b) => add(a, b), 0);
}
