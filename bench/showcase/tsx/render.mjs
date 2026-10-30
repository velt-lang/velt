// render.vlt on React (react-dom/server renderToString) or Preact (preact-render-to-string):
// the same tree (page.mjs), data, sizes and RESULT lines.
//
//   node render.mjs <react|preact> [both|small|large] [scale]
//   node render.mjs <react|preact> html <small|large>
// React runs in production mode (NODE_ENV=production, set by bench/showcase), as it is deployed.
import { items, load } from "./page.mjs";

const { render } = await load(process.argv[2]);

function hash(s) {
  let h = 5381;
  for (let i = 0; i < s.length; i++) {
    h = (h * 33 + s.charCodeAt(i)) % 1000000007;
  }
  return h;
}

function bench(name, rows, renders) {
  const data = items(rows);
  const title = `${name} page`;
  let total = 0;
  let last = "";
  const t0 = performance.now();
  for (let r = 0; r < renders; r++) {
    last = render(title, data);
    total += last.length;
  }
  const ms = performance.now() - t0;
  console.log(`RESULT tsx.${name} ${renders} ${(total + hash(last)) % 1000000007} ${ms}`);
}

const which = process.argv[3] ?? "both";
if (which === "html") {
  const big = process.argv[4] === "large";
  console.log(render(big ? "large page" : "small page", items(big ? 1000 : 10)));
} else {
  const scale = process.argv[4] ? parseInt(process.argv[4], 10) : 1;
  if (which === "small" || which === "both") bench("small", 10, Math.trunc(20000 / scale));
  if (which === "large" || which === "both") bench("large", 1000, Math.trunc(500 / scale));
}
