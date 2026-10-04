// Prelude functions and types that TypeScript doesn't have; names the file declares itself
// are fine.

export function same(a: number[], b: number[]): boolean {
  return deepEqual(a, b); //~ velt-global
}

export function check(ok: boolean): void {
  assert(ok, "not ok"); //~ velt-global
}

export function value(v: JsonValue): string { //~ velt-global
  return v.asString() ?? ""; //~ velt-member
}

function isEven(n: number): boolean {
  return n % 2 === 0;
}

export function evens(xs: number[]): number[] {
  return xs.filter(isEven);
}

function mayThrow(s: string): number {
  if (s === "") {
    throw new Error("empty");
  }
  return 1;
}

export function attempted(s: string): boolean {
  const r = attempt(() => mayThrow(s)); //~ velt-global
  return r instanceof Error;
}

async function save(): Promise<void> {}

// The timer functions are TypeScript's (the DOM's); the handle's type and members are Velt's.
export function later(): void {
  const t = setTimeout(() => save(), 10);
  clearTimeout(t);
  const i = setInterval(() => save(), 10);
  clearInterval(i);
}

export function cancel(t: Timer): void { //~ velt-global
  t.clear(); //~ velt-member
  t.unref(); //~ velt-member
}
