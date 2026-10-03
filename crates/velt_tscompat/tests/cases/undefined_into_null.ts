// A value that is `undefined` in JavaScript doesn't fit a declared `T | null` in TypeScript.

export function find(m: Map<string, number>, key: string): number | null {
  return m.get(key); //~ undefined-into-null
}

export function first(xs: string[]): string | null {
  const x: string | null = xs.find((s) => s.length > 0); //~ undefined-into-null string-offsets
  return x;
}

export type Slot = { value: number | null };

export function slot(m: Map<string, number>): Slot {
  return { value: m.get("a") }; //~ undefined-into-null
}

export function loose(m: Map<string, number>): number {
  const v = m.get("a");
  return v ?? 0;
}
