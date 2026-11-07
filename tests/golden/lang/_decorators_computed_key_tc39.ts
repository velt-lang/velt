// Valid TypeScript: `d` is an ordinary function, a TC39 run-time decorator (#2204).
function d(...a: any[]): any {}
const k = "x";
export class C {
  @d [k] = 1;
}
export class R {
  @d readonly [k]: number = 1;
}
