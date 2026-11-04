import { early, also } from "./_let_read_before_init_always_b";

function make(): number {
  return 5;
}

export let v = make();

export function get(): number {
  return early + also;
}
