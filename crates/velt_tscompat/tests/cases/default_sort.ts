// `sort()` without a comparator compares numbers as text in JavaScript.

export function sorted(xs: number[]): number[] {
  xs.sort(); //~ default-sort
  return xs.toSorted(); //~ default-sort
}

export function names(xs: string[]): string[] {
  return xs.toSorted();
}

export function byValue(xs: number[]): number[] {
  return xs.toSorted((a, b) => a - b);
}
