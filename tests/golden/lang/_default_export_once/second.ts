// Runs before `first.ts`, which imports it, and calls `first.ts`'s default function.
import hoisted from "./first";

console.log("second runs first:", hoisted());
export const fromSecond: string = "second";
