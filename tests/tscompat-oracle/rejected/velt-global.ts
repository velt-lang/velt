// `deepEqual` is the Velt prelude's; TypeScript has no such global.

export function same(a: number[], b: number[]): boolean {
  return deepEqual(a, b);
}
