// Tagged unions (same workload as shapes.vlt): discriminated objects and `switch`.
function area(s) {
  switch (s.kind) {
    case "circle":
      return 3.0 * s.r * s.r;
    case "rect":
      return s.w * s.h;
    case "tri":
      return 0.5 * s.b * s.h;
    case "empty":
      return 0.0;
  }
}

function make(i) {
  const x = ((i * 7919) % 1000) * 0.001;
  switch ((i * 31) % 4) {
    case 0:
      return { kind: "circle", r: x };
    case 1:
      return { kind: "rect", w: x, h: 2.0 };
    case 2:
      return { kind: "tri", b: x, h: 4.0 };
    default:
      return { kind: "empty" };
  }
}

const shapes = [];
for (let i = 0; i < 1000000; i++) shapes.push(make(i));
let total = 0.0;
let empties = 0;
for (let pass = 0; pass < 40; pass++) {
  for (const s of shapes) {
    total += area(s);
    if (s.kind === "empty") empties += 1;
  }
}
console.log(Math.floor(total), empties);
