// `/` divides integers in Velt when both operands have integer types; TypeScript has only
// `number`. Integer types come only from Velt's names (`i64`, reported on their own): a union
// of integer literal types is a JS number and divides as numbers do (#2686).

export function half(xs: (1 | 3)[]): string {
  return `${xs[0] / 2}`;
}

export function average(total: number, count: number): number {
  return total / count;
}

export function truncated(xs: (1 | 3)[]): number {
  const n = xs.length;
  return Math.trunc(n / 2);
}
