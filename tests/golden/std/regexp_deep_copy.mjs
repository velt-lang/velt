// The Node twin of tests/golden/std/regexp_deep_copy.velt; its .out is what Node 22 prints. A
// spawned task and a channel are a worker's boundary in Velt, where the value is copied as
// `postMessage` copies it: `structuredClone` stands in for that copy here.
const s = "aaaa";

function both(label, a, b) {
  a.exec(s);
  a.exec(s);
  b.exec(s);
  console.log(label, a.lastIndex, b.lastIndex);
}

async function inTask(r) {
  r.exec(s);
  return r.lastIndex;
}

async function searches(re, n) {
  let sum = 0;
  for (let i = 0; i < n; i++) {
    re.lastIndex = 0;
    re.exec("xaab aab");
    sum += re.lastIndex;
    sum += re.test("aab") ? 1 : 0;
  }
  return sum;
}

async function main() {
  const g = /a/g;
  g.lastIndex = 1;
  const gc = structuredClone(g);
  console.log("clone g", gc.lastIndex, gc.source, gc.flags, gc === g);
  gc.lastIndex = 3;
  console.log("write copy", g.lastIndex, gc.lastIndex);
  both("exec g", g, gc);

  const y = /a/y;
  y.lastIndex = 2;
  const yc = structuredClone(y);
  console.log("clone y", yc.lastIndex);
  both("exec y", y, yc);

  const holder = { name: "h", re: /a/g, list: [/a/y, /b/gi] };
  holder.re.lastIndex = 2;
  holder.list[0].lastIndex = 1;
  const hc = structuredClone(holder);
  console.log("in object", hc.re.lastIndex, hc.list[0].lastIndex, hc.list[1].flags);
  hc.re.exec(s);
  console.log("object after exec", holder.re.lastIndex, hc.re.lastIndex);

  const t = /a/g;
  t.lastIndex = 1;
  const fromTask = await inTask(structuredClone(t));
  t.exec(s);
  console.log("spawned", fromTask, t.lastIndex);

  const sent = /a/y;
  sent.lastIndex = 1;
  const got = structuredClone(sent);
  got.exec(s);
  sent.exec(s);
  console.log("channel", got.lastIndex, sent.lastIndex, got === sent);

  const word = /a+b/g;
  word.lastIndex = 2;
  const ps = [];
  for (let k = 0; k < 4; k++) {
    ps.push(searches(structuredClone(word), 2000));
  }
  const sums = await Promise.all(ps);
  console.log("threads", sums.join(" "), word.lastIndex);
}

main();
