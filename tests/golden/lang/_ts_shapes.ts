// A TypeScript module imported by `ts_modules.vlt`.

export type Rect = { width: number; height: number };

export function area(r: Rect): number {
  return r.width * r.height;
}

export function perimeter(r: Rect): number {
  return 2 * (r.width + r.height);
}
