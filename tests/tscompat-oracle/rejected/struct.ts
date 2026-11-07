// `struct` declarations are Velt-only syntax.

export struct Point {
  x: number;
  y: number;
}

export type Size = { w: number; h: number };

export function origin(): Point {
  return { x: 0, y: 0 };
}
