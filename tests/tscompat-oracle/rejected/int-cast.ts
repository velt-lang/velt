// `as` to an integer type converts in Velt; `tsc` doesn't know the type.

export function whole(x: number): number {
  const n = x as i64;
  return (n as number) + 1;
}

export function narrow(x: number): number {
  return (x as u8) as number;
}
