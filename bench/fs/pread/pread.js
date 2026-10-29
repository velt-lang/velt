// pread.vlt's workloads in Node: fs.readSync / writeSync with a position, fsyncSync.
const fs = require("node:fs");

const PAGE = 4096;
const READS = 200000;
const PASSES = 8;
const SYNCS = 200;

function report(name, ms, check) {
  console.log(`${name} ${ms.toFixed(1)} ${check}`);
}

function main() {
  const path = process.argv[2];
  const fd = fs.openSync(path, "r");
  const size = fs.fstatSync(fd).size;
  const pages = Math.floor(size / PAGE);
  const page = Buffer.alloc(PAGE);

  let x = 2463534242;
  let sum = 0;
  let start = performance.now();
  for (let i = 0; i < READS; i++) {
    x = (x ^ (x << 13)) >>> 0;
    x = (x ^ (x >>> 17)) >>> 0;
    x = (x ^ (x << 5)) >>> 0;
    const at = x % pages;
    const n = fs.readSync(fd, page, 0, PAGE, at * PAGE);
    sum += n + page[0];
  }
  report("random_4k", performance.now() - start, sum);

  const chunk = Buffer.alloc(65536);
  sum = 0;
  start = performance.now();
  for (let p = 0; p < PASSES; p++) {
    let at = 0;
    while (true) {
      const n = fs.readSync(fd, chunk, 0, 65536, at);
      if (n === 0) break;
      at += n;
      sum += n + chunk[n - 1];
    }
  }
  report("sequential_64k", performance.now() - start, sum);
  fs.closeSync(fd);

  const wpath = `${path}.w`;
  const w = fs.openSync(wpath, "w");
  const rec = Buffer.alloc(100);
  start = performance.now();
  for (let i = 0; i < SYNCS; i++) {
    rec[0] = i & 255;
    fs.writeSync(w, rec, 0, 100, i * 100);
    fs.fsyncSync(w);
  }
  report("pwrite_fsync", performance.now() - start, fs.fstatSync(w).size);
  fs.closeSync(w);
  fs.unlinkSync(wpath);
}

main();
