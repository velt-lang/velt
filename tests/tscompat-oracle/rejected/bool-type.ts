// `bool` is Velt's other name for `boolean`; `tsc` doesn't know it.

export function both(a: bool, b: boolean): boolean {
  return a && b;
}

export function flags(): bool[] {
  return [true, false];
}
