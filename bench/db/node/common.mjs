// Shared by the Node baselines: the RESULT line format, the typed row and the size scale.
import { performance } from "node:perf_hooks";

export const now = () => performance.now();

// 20 when the first argument is `quick` (1/20 of the sizes), else 1.
export const scale = process.argv[2] === "quick" ? 20 : 1;

export function report(workload, ops, checksum, ms) {
  console.log(`RESULT ${workload} ${ops} ${checksum} ${ms}`);
}

// The typed row every implementation decodes into.
export class Row {
  constructor(id, name, score) {
    this.id = id;
    this.name = name;
    this.score = score;
  }
}

export function rowChecksum(r) {
  return r.id + r.name.length + Math.trunc(r.score * 2);
}
