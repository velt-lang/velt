// Union parse (see parse_union.vlt): `JSON.parse` and a switch on `kind`.
const parts = [];
for (let i = 0; i < 200000; i++) {
  if (i % 2 == 0) {
    parts.push(`{"kind":"circle","r":${i % 100}.5}`);
  } else {
    parts.push(`{"w":${i % 100},"h":${i % 7},"kind":"rect"}`);
  }
}
const text = `[${parts.join(",")}]`;
let sum = 0;
for (let round = 0; round < 8; round++) {
  for (const s of JSON.parse(text)) {
    switch (s.kind) {
      case "circle":
        sum += Math.trunc(s.r * 2);
        break;
      case "rect":
        sum += Math.trunc(s.w * s.h);
        break;
    }
  }
}
console.log(text.length, sum);
