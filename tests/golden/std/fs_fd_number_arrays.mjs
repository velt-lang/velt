// The Node twin of tests/golden/std/fs_fd_number_arrays.velt: the same calls on typed arrays.
import * as fs from "node:fs";

function attempt(f) {
  try {
    console.log(f());
  } catch (e) {
    console.log(e.name, e.message);
  }
}

function f32s(xs) {
  return Array.from(xs).join(",");
}

function main() {
  const path = "velt_fs_fd_number_arrays_tmp.bin";
  const w = fs.openSync(path, "w+");
  const fs4 = new Float32Array([1.5, -2.25, 3, 0.5]);
  console.log("write f32", fs.writeSync(w, fs4));
  const small = new Int8Array([-1, 2, -3, 127, -128]);
  console.log("write i8", fs.writeSync(w, small, 0, 5, 16));
  const words = new Uint32Array([0x04030201, 0xdeadbeef, 0x0c0b0a09]);
  console.log("write u32 part", fs.writeSync(w, words, 4, 8, 21));
  console.log("file", Array.from(fs.readFileSync(path)).join(","));

  const back = new Float32Array(4);
  console.log("read f32", fs.readSync(w, back, 0, 16, 0), f32s(back));
  const codes = new Int8Array(6);
  console.log("read i8", fs.readSync(w, codes, 1, 5, 16), codes.join(","));
  const ids = new Uint32Array(3);
  console.log("read u32", fs.readSync(w, ids, 4, 8, 21), ids.join(","));
  const tail = new Float64Array([7, 7]);
  console.log("read f64 at end", fs.readSync(w, tail, 0, 16, 21), tail[1]);
  const halves = new Float32Array(2);
  console.log("read f32 unaligned", fs.readSync(w, halves, 2, 4, 0), f32s(halves));
  const r = fs.openSync(path, "r");
  const first = new Uint32Array(2);
  console.log("read u32 at fd position", fs.readSync(r, first), first.join(","));
  console.log("and the next", fs.readSync(r, first, 0, 4), first.join(","));

  attempt(() => `${fs.readSync(r, new Float32Array(2), 0, 9, 0)}`);
  attempt(() => `${fs.readSync(r, new Float32Array(2), 9, 0, 0)}`);
  attempt(() => `${fs.readSync(r, new Float64Array(0), 0, 1, 0)}`);
  attempt(() => `${fs.writeSync(w, new Uint32Array([1, 2]), 2, 7, 0)}`);
  fs.closeSync(r);
  fs.closeSync(w);
  fs.unlinkSync(path);
}
main();
