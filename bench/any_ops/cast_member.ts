// Field reads of a union value asserted to one member, `(c as V).f`, in a loop: the member held,
// read as a value and in a test (cast_member.rs). `bench/any_ops/count.sh … cast_member.ts`;
// cast_member_boxonce.ts is the same loop in a program that also converts an element to `any`.
type V = { type: string; props: number };
type C = V | string | number | C[];
function main(): void {
  const items: C[] = [];
  for (let i = 0; i < 1000; i++) items.push({ type: "t" + (i % 7), props: i });
  let n = 0;
  for (let r = 0; r < 2000; r++) {
    for (const c of items) {
      n += (c as V).props;
      if ((c as V).type) n += 1;
    }
  }
  console.log(n);
}
main();
