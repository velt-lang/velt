// The Node twin of tests/golden/std/fs_line_reads.velt. Node has no reader with `readLine`, so a
// small one over the file's bytes stands for Velt's (lines split at "\n" only, a "\r" before it
// dropped, text decoded as Node's `toString("utf8")` decodes it); the two tasks share one
// `readline` interface's iterator, Node's own concurrent line reads.
import * as fs from "node:fs";
import * as os from "node:os";
import * as readline from "node:readline";
import { randomUUID } from "node:crypto";

function fileOf(parts) {
  const path = `${os.tmpdir()}/node-fs-lines-${randomUUID()}.txt`;
  fs.writeFileSync(path, Buffer.concat(parts.map((p) => Buffer.from(p))));
  return path;
}

const text = (s) => Buffer.from(s);
const repeat = (byte, n) => Buffer.alloc(n, byte);

// The length of the UTF-8 character lead byte `b` starts (1 for a byte that starts none).
const charLength = (b) => (b < 0xc0 ? 1 : b >= 0xf0 ? 4 : b >= 0xe0 ? 3 : 2);

class Reader {
  constructor(path) {
    this.path = path;
    this.data = fs.readFileSync(path);
    this.at = 0;
    this.closed = false;
  }
  close() {
    this.closed = true;
  }
  readLine() {
    if (this.closed) {
      const e = new Error("bad file descriptor");
      e.code = "EBADF";
      throw e;
    }
    if (this.at >= this.data.length) {
      // The end of the file was reported: look again, the file may have grown.
      this.data = fs.readFileSync(this.path);
      if (this.at >= this.data.length) return null;
    }
    let nl = this.data.indexOf(10, this.at);
    let next = nl + 1;
    if (nl < 0) {
      nl = this.data.length;
      next = nl;
    } else if (nl > this.at && this.data[nl - 1] === 13) {
      nl -= 1;
    }
    const line = this.data.toString("utf8", this.at, nl);
    this.at = next;
    return line;
  }
  read(n) {
    const out = [...this.data.subarray(this.at, this.at + n)];
    this.at += out.length;
    return out;
  }
  // Up to `max` bytes (0: 64 KiB) as text, cut before a character split at their end; a
  // character longer than `max` comes whole.
  readString(max = 0) {
    const want = max === 0 ? 65536 : max;
    let stop = Math.min(this.at + want, this.data.length);
    let i = stop;
    while (i > this.at && stop - i < 4) {
      i--;
      if ((this.data[i] & 0xc0) !== 0x80) {
        if (i + charLength(this.data[i]) > stop) stop = i;
        break;
      }
    }
    if (stop === this.at && this.at < this.data.length) {
      stop = Math.min(this.at + charLength(this.data[this.at]), this.data.length);
    }
    const s = this.data.toString("utf8", this.at, stop);
    this.at = stop;
    return s;
  }
}

function allLines(path) {
  const r = new Reader(path);
  const out = [];
  for (let line = r.readLine(); line !== null; line = r.readLine()) out.push(line);
  return out;
}

async function main() {
  const paths = [];

  const basic = fileOf([text("a\r\nb\n\nc\rd\nlast")]);
  paths.push(basic);
  console.log(JSON.stringify(allLines(basic)));

  const empty = fileOf([]);
  paths.push(empty);
  console.log(JSON.stringify(allLines(empty)));

  const one = fileOf([text("x\n")]);
  paths.push(one);
  console.log(JSON.stringify(allLines(one)));

  const bad = fileOf([[97, 255, 98, 10, 195, 10]]);
  paths.push(bad);
  console.log(JSON.stringify(allLines(bad)));

  const long = fileOf([repeat(122, 70000), text("\n"), repeat(121, 65535), text("\r\nafter")]);
  paths.push(long);
  const longLines = allLines(long);
  console.log(longLines.map((l) => l.length), longLines[1].endsWith("\r"), longLines[2]);

  const mixed = fileOf([text("one\ntwo\nthree\nfour")]);
  paths.push(mixed);
  const m = new Reader(mixed);
  console.log(m.readLine());
  console.log(m.read(3));
  console.log(JSON.stringify(m.readLine()));
  console.log(JSON.stringify(m.readString()));
  console.log(m.readLine());

  const split = fileOf([text("x\n"), repeat(97, 65533), text("éend")]);
  paths.push(split);
  const s = new Reader(split);
  console.log(s.readLine());
  let rest = "";
  for (let piece = s.readString(); piece !== ""; piece = s.readString()) rest += piece;
  console.log(rest.length, rest.endsWith("aéend"), rest.indexOf("�"));

  const held = fileOf([repeat(120, 65535), text("\n"), repeat(98, 65535), text("é\nend")]);
  paths.push(held);
  const h = new Reader(held);
  const xs = h.readLine();
  const bs = h.readString();
  console.log(xs?.length, bs.length, bs.indexOf("�"), h.readLine(), h.readLine());

  const narrow = fileOf([text("x\néab")]);
  paths.push(narrow);
  const n = new Reader(narrow);
  console.log(n.readLine(), n.readString(1), n.readString(1), n.read(5));

  const grows = fileOf([text("a\n")]);
  paths.push(grows);
  const g = new Reader(grows);
  console.log(g.readLine(), g.readLine());
  fs.appendFileSync(grows, "b\n");
  console.log(g.readLine(), g.readLine());
  g.close();
  try {
    g.readLine();
  } catch (e) {
    console.log("after close:", e.code);
  }

  const early = fileOf([text("aéb\nc\n")]);
  paths.push(early);
  const e = new Reader(early);
  console.log(e.readString(2), e.readLine(), e.readLine());

  // Lines and 777-byte reads in turn on one reader (Node has no copies to read from two tasks).
  const mixedLines = [];
  let mixedSize = 0;
  for (let i = 0; i < 60000; i++) {
    const l = text(`line ${i} ${"-".repeat(i % 40)}\n`);
    mixedSize += l.length;
    mixedLines.push(l);
  }
  const mixedFile = fileOf(mixedLines);
  paths.push(mixedFile);
  const mr = new Reader(mixedFile);
  let counted = 0;
  for (;;) {
    const line = mr.readLine();
    if (line === null) break;
    counted += Buffer.byteLength(line) + 1;
    counted += mr.read(777).length;
  }
  console.log("mixed tasks:", counted === mixedSize);

  const two = fileOf([text("one\ntwo\nthree\n")]);
  paths.push(two);
  const r1 = new Reader(two);
  const r2 = new Reader(two);
  console.log(r1.readLine(), r2.readLine(), r1.readLine(), r2.readLine());

  const lines = [];
  for (let i = 0; i < 100000; i++) lines.push(text(`line ${i} ${"-".repeat(i % 50)}\n`));
  const shared = fileOf(lines);
  paths.push(shared);
  const rl = readline.createInterface({ input: fs.createReadStream(shared), crlfDelay: Infinity });
  const it = rl[Symbol.asyncIterator]();
  async function readAll() {
    const out = [];
    for (;;) {
      const { value, done } = await it.next();
      if (done) return out;
      out.push(value);
    }
  }
  const got = await Promise.all(Array.from({ length: 8 }, () => readAll()));
  const seen = new Set();
  let total = 0;
  let ordered = true;
  let whole = true;
  for (const mine of got) {
    let prev = -1;
    for (const line of mine) {
      total++;
      const parts = line.split(" ");
      if (parts.length !== 3 || parts[0] !== "line") {
        whole = false;
        continue;
      }
      const n = parseInt(parts[1]);
      if (parts[2] !== "-".repeat(n % 50)) whole = false;
      if (n <= prev) ordered = false;
      prev = n;
      seen.add(line);
    }
  }
  console.log(total, seen.size, whole, ordered);
  rl.close();

  for (const p of paths) fs.rmSync(p);
}

main();
