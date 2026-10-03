// Prelude functions and types that TypeScript doesn't have; names the file declares itself
// are fine.

export function same(a: number[], b: number[]): boolean {
  return deepEqual(a, b); //~ velt-global
}

export function check(ok: boolean): void {
  assert(ok, "not ok"); //~ velt-global
}

export function value(v: JsonValue): string { //~ velt-global
  return v.asString() ?? ""; //~ velt-member
}

function isEven(n: number): boolean {
  return n % 2 === 0;
}

export function evens(xs: number[]): number[] {
  return xs.filter(isEven);
}
