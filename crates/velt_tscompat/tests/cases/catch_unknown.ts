// TypeScript types a caught value `unknown`: narrow it before using its members.

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
    console.log(e.message); //~ catch-unknown
    return 0;
  }
}

export function narrowed(s: string): string {
  try {
    check(s);
    return "";
  } catch (e) {
    if (e instanceof Invalid) {
      return e.message;
    }
    return "?";
  }
}
