// A TypeScript module imported by `simd_names.vlt`: it declares types named like Velt's SIMD
// types, which TypeScript code could do before they existed. Its own declarations win.

export type f32x4 = [number, number, number, number];

export function dot(a: f32x4, b: f32x4): number {
  return a[0] * b[0] + a[1] * b[1] + a[2] * b[2] + a[3] * b[3];
}

export class u8x16 {
  bytes: number[] = [];

  static splat(x: number): u8x16 {
    const v = new u8x16();
    for (let i = 0; i < 16; i++) v.bytes.push(x);
    return v;
  }
}

export interface i32x4 {
  lanes: number[];
}

export enum mask8x16 {
  None,
  All,
}

export function describe(m: mask8x16, v: i32x4): string {
  return `${m === mask8x16.All ? "all" : "none"} of ${v.lanes.length}`;
}
