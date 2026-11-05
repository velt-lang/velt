import { fromSecond } from "./second";

export default function hoisted(): string {
  return "hoisted";
}

export function first(): string {
  return fromSecond;
}
