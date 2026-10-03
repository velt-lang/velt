// Members of standard types that TypeScript doesn't have.

export function empty(xs: number[]): boolean {
  return xs.isEmpty(); //~ velt-member
}

export function orZero(x: number | null): number {
  return x.unwrapOr(0); //~ velt-member
}

export function bump(m: Map<string, number>, key: string): void {
  m.upsert(key, 1, (v) => v + 1); //~ velt-member
}

export function parse(s: string): { a: number } {
  return JSON.parse<{ a: number }>(s); //~ velt-member
}

export class Box {
  isEmpty(): boolean {
    return true;
  }
}

export function standard(xs: number[], b: Box): boolean {
  return xs.includes(1) && b.isEmpty() && Math.max(1, 2) > 0;
}

export class Point {
  x: number = 0;
}

export class Copyable {
  n: number = 1;

  clone(): Copyable {
    return new Copyable();
  }
}

export function copies(p: Point, c: Copyable, m: Map<string, number>): number {
  const q = p.clone(); //~ velt-member
  const n = m.clone(); //~ velt-member
  const d = c.clone();
  return q.x + n.size + d.n;
}

export function generic<T>(x: T): T {
  return x.clone(); //~ velt-member
}

export function deferred(): Promise<number> {
  return Promise.withResolvers<number>().promise; //~ velt-member
}
