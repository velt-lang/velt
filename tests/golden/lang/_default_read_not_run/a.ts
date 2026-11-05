import { early, earlyNamed, earlyLogical } from "./b";

function make(): number {
  return 5;
}

export default make();

export let v = make();

export function getDefault(): number {
  return early;
}

export function getNamed(): number {
  return earlyNamed;
}

export function getLogical(): boolean {
  return earlyLogical;
}
