// TypeScript's `keys()` returns an iterator, which has no `length`.

export function count(m: Map<string, number>): number {
  return m.keys().length;
}
