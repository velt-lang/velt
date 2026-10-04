// `isEmpty` is a Velt array method, `clone` the compiler's on every type, and
// `Promise.withResolvers` is ES2024; TypeScript's baseline has none of them.

export function empty(xs: number[]): boolean {
  return xs.isEmpty();
}

export class Point {
  x: number = 0;
}

export function copy(p: Point): Point {
  return p.clone();
}

export function deferred(): Promise<number> {
  return Promise.withResolvers<number>().promise;
}
