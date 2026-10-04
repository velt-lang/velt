// `deepEqual` is the Velt prelude's and `attempt` a builtin; TypeScript has no such globals.

export function same(a: number[], b: number[]): boolean {
  return deepEqual(a, b);
}

function mayThrow(s: string): number {
  if (s === "") {
    throw new Error("empty");
  }
  return 1;
}

export function attempted(s: string): boolean {
  return attempt(() => mayThrow(s)) instanceof Error;
}
