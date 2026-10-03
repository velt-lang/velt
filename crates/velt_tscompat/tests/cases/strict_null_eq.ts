// `=== null` misses JavaScript's `undefined`: optional fields and parameters left out,
// `Map.get` of a missing key, `find` without a match, `pop` of an empty array, `?.`.

export type User = { name: string; email?: string; manager: string | null };

export function hasEmail(u: User): boolean {
  return u.email !== null; //~ strict-null-eq
}

export function hasManager(u: User): boolean {
  return u.manager !== null;
}

export function greet(name: string, greeting?: string): string {
  if (greeting === null) { //~ strict-null-eq
    return name;
  }
  return `${greeting} ${name}`;
}

export function lookup(m: Map<string, number>, key: string): number {
  const found = m.get(key);
  if (found === null) { //~ strict-null-eq
    return 0;
  }
  return found;
}

export function firstBig(xs: number[]): boolean {
  return null === xs.find((x) => x > 10); //~ strict-null-eq
}

export function last(xs: string[]): boolean {
  return xs.pop() == null;
}

export function managerLength(u: User | null): boolean {
  return u?.manager === null; //~ strict-null-eq
}
