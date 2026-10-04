// A length is unsigned in Velt: `[].length - 1` wraps around, where JavaScript gives -1.

export function main() {
  const xs: string[] = [];
  console.log(xs.length - 1);
}
