// TypeScript types a caught value `unknown` in strict mode.

export class Invalid extends Error {}

export function check(s: string): number {
  if (s === "") {
    throw new Invalid("empty");
  }
  return 1;
}

export function parse(s: string): number {
  try {
    return check(s);
  } catch (e) {
    console.log(e.message);
    return 0;
  }
}
