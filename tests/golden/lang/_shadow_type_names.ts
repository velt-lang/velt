// A TypeScript module that declares every type name Velt has and TypeScript does not, as
// binary parsers and math libraries may (#1074). Its own declarations win over Velt's types.

type u8 = number;
type u16 = number;
type u32 = number;
type u64 = boolean;
type usize = string;
type i8 = { value: number };
export class i16 {
  constructor(public value: number) {}
}
export interface i32 {
  hi: number;
  lo: number;
}
export enum i64 {
  Low,
  High,
}
type isize = number[];
export class f32 {
  constructor(public bits: number) {}
  twice(): f32 {
    return new f32(this.bits * 2);
  }
}
type f64 = "f64";
export enum bool {
  No,
  Yes,
}

export function describe(): string {
  const bytes: u8[] = [1.5, 2.5, 300];
  const half: u16 = 0.5;
  const neg: u32 = -1;
  const wide: u64 = true;
  const label: usize = "len";
  const boxed: i8 = { value: 7 };
  const pair: i32 = { hi: 1, lo: 2 };
  const list: isize = [3, 4];
  const tag: f64 = "f64";
  const sum = bytes.reduce((a, b) => a + b, 0);
  return `${sum} ${half} ${neg} ${wide} ${label} ${boxed.value} ${new i16(-3).value} ${pair.hi + pair.lo} ${i64.High} ${list.length} ${new f32(1.5).twice().bits} ${tag} ${bool.Yes}`;
}
