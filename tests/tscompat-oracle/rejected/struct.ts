// `struct` declarations and named object literals are Velt-only syntax.

export struct Point {
  x: number;
  y: number;
}

export type Size = { w: number; h: number };

export function origin(): Point {
  return Point { x: 0, y: 0 };
}
