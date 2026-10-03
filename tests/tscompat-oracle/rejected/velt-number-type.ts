// `tsc` doesn't know Velt's number type names.

export function scale(x: f64, by: i32): number {
  return x * 2 + (by as number);
}

export type Pixel = { r: u8; g: u16; b: usize };

export function sizes(xs: Map<string, u64[]>): f32 {
  return xs.size as f32;
}
