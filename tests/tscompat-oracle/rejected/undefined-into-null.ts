// TypeScript types `Map.get` as `number | undefined`, which a `number | null` doesn't accept.

export function find(m: Map<string, number>, key: string): number | null {
  return m.get(key);
}
