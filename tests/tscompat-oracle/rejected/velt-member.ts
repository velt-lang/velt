// `isEmpty` is a Velt array method; TypeScript's arrays don't have it.

export function empty(xs: number[]): boolean {
  return xs.isEmpty();
}
