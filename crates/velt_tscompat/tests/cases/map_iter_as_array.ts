// `keys()`, `values()` and `entries()` are arrays in Velt, iterators in TypeScript.

export function count(m: Map<string, number>): number {
  return m.keys().length; //~ map-iter-as-array
}

export function firstValue(m: Map<string, number>): number {
  return m.values()[0]; //~ map-iter-as-array
}

export function names(m: Map<string, number>): string[] {
  return m.keys(); //~ map-iter-as-array
}

export function total(m: Map<string, number>): number {
  let sum = 0.0;
  for (const v of m.values()) {
    sum += v;
  }
  return sum + [...m.keys()].length;
}
