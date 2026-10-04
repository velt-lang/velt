// A destructuring default replaces `null` in Velt, but only `undefined` in JavaScript (#431).
// Optional fields are left out (`undefined`) in JavaScript, so their defaults agree.

export type Pt = { x: number | null; y: number };

export function sum(p: Pt): number {
  const { x = 1.0, y = 0.0 } = p; //~ null-default
  return x + y;
}

export type Size = { w: number | null; h: number | null; unit?: string };

export function area(s: Size): string {
  const { w = 0.0, h = 0.0 } = s; //~ null-default null-default
  const { unit = "px" } = s;
  return `${w * h}${unit}`;
}
