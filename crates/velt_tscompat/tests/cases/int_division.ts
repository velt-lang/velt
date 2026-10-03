// `/` divides integers in Velt when both operands have integer types; TypeScript has only
// `number`. Integer types come from Velt's names (`i64`, reported on their own) or from unions
// of integer literal types.

export function half(xs: (1 | 3)[]): string {
  return `${xs[0] / 2}`; //~ int-division
}

export function average(total: number, count: number): number {
  return total / count;
}

export function truncated(xs: (1 | 3)[]): number {
  const n = xs.length;
  return Math.trunc(n / 2);
}
