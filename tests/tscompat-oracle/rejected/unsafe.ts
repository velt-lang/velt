// `unsafe` marks Velt code that calls native code; TypeScript has no such keyword, so `tsc`
// reads `unsafe` as an identifier and stops at what follows it.

// SAFETY: callers pass any number.
export unsafe function raw(n: number): number {
  return n;
}

export function caller(n: number): number {
  // SAFETY: any number will do.
  unsafe {
    n = raw(n);
  }
  // SAFETY: as above.
  return unsafe { raw(n) };
}
