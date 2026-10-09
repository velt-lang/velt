// End-to-end check of the sigx + Velt packaging POC. Needs `pnpm install`, `velt` on PATH (or
// $VELT) and Playwright's Chromium. Run `pnpm build` first. Steps:
//   1. bytes:   Velt's HTML for each page equals JavaScript sigx's (scripts/reference.mjs)
//   2. prod:    dist/server/app serves the client build and the page hydrates (clicks work, no
//               console warnings)
//   3. dev/HMR: under `vite`, a shared component edit hot-updates the browser (state kept) and
//               the server; a server-only edit reloads the page; a Velt compile error shows in
//               Vite's overlay while the old server keeps serving.
import { spawn, execFileSync } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import { createServer } from "node:net";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright";

const root = fileURLToPath(new URL("..", import.meta.url));
const PAGES = ["/", "/about"];
let failed = 0;
const check = (ok, what) => {
  console.log(`${ok ? "ok  " : "FAIL"} ${what}`);
  if (!ok) failed++;
};
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const freePort = () =>
  new Promise((ok) => {
    const s = createServer().listen(0, "127.0.0.1", () => {
      const { port } = s.address();
      s.close(() => ok(port));
    });
  });

// Starts `cmd`, resolves once a line of its output matches `ready`.
function start(cmd, args, ready, env = {}) {
  const child = spawn(cmd, args, { cwd: root, env: { ...process.env, ...env } });
  let log = "";
  const p = new Promise((ok, fail) => {
    const on = (d) => {
      log += d;
      if (ready.test(log)) ok(child);
    };
    child.stdout.on("data", on);
    child.stderr.on("data", on);
    child.on("exit", (code) => fail(new Error(`${cmd} exited (${code}):\n${log}`)));
  });
  child.log = () => log;
  return p;
}

async function until(fn, ms = 10000) {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    // A check may run while the page reloads (its context is destroyed): not yet.
    if (await fn().catch(() => false)) return true;
    await sleep(100);
  }
  return false;
}

const outlet = (html) => html.slice(html.indexOf('<div id="app">') + 14, html.lastIndexOf("</div>"));

// --- 1 + 2: production binary ---
const port = await freePort();
const prod = await start("./dist/server/app", ["--port", String(port)], /listening/);
const browser = await chromium.launch();
try {
  for (const path of PAGES) {
    const ref = execFileSync("node", ["scripts/reference.mjs", path], { cwd: root, encoding: "utf8" });
    const html = await (await fetch(`http://127.0.0.1:${port}${path}`)).text();
    check(outlet(html) === ref, `bytes: Velt's ${path} equals JavaScript sigx's`);
    if (outlet(html) !== ref) console.log(`  velt: ${outlet(html)}\n  node: ${ref}`);
  }
  const asset = readFileSync(`${root}/dist/client/index.html`, "utf8").match(/src="(\/assets\/[^"]+)"/)[1];
  const a = await fetch(`http://127.0.0.1:${port}${asset}`);
  check(
    a.status === 200 && a.headers.get("content-type").startsWith("text/javascript") &&
      a.headers.get("cache-control").includes("immutable"),
    `prod: ${asset} served with JavaScript type and immutable cache`,
  );

  const page = await browser.newPage();
  const messages = [];
  page.on("console", (m) => (m.type() === "warning" || m.type() === "error") && messages.push(m.text()));
  page.on("pageerror", (e) => messages.push(String(e)));
  await page.goto(`http://127.0.0.1:${port}/`, { waitUntil: "networkidle" });
  await page.click("#inc");
  await page.click("#inc");
  check(
    await until(async () => (await page.textContent(".card p")).startsWith("Count: 3 (doubled 6)")),
    "prod: hydrated, two clicks give Count: 3",
  );
  check(messages.length === 0, `prod: no console warnings or errors${messages.length ? `: ${messages.join(" | ")}` : ""}`);
  await page.close();
} finally {
  prod.kill();
}

// --- 3: dev server and HMR ---
const counterFile = `${root}/src/shared/Counter.tsx`;
const serverFile = `${root}/src/server.vlt`;
const counterSrc = readFileSync(counterFile, "utf8");
const serverSrc = readFileSync(serverFile, "utf8");
const devPort = await freePort();
const dev = await start("./node_modules/.bin/vite", ["--port", String(devPort), "--strictPort"], /sigx: listening/);
const url = `http://localhost:${devPort}/`;
const get = async () => (await fetch(url, { headers: { accept: "text/html" } })).text();
try {
  const html = await get();
  check(html.includes("/@vite/client") && html.includes("<!--$c:1-->"), "dev: Velt renders the page, Vite injects its client");

  const page = await browser.newPage();
  await page.goto(url, { waitUntil: "networkidle" });
  await page.click("#inc");
  await page.evaluate(() => (window.__marker = 1)); // gone after a full reload
  check(await until(async () => (await page.textContent(".card p")).startsWith("Count: 2")), "dev: hydrated");

  // A shared component: Vite's HMR updates the browser in place, velt dev the server.
  writeFileSync(counterFile, counterSrc.replace(/>\s*\+1\s*</, ">add one<"));
  check(await until(async () => (await page.textContent("#inc")) === "add one"), "dev: shared edit hot-updates the browser");
  check(await page.evaluate(() => window.__marker === 1), "dev: ... without a full reload");
  check((await get()).includes(">add one</button>"), "dev: ... and the server renders the new markup");

  // Server-only code: the page reloads with the server's new HTML.
  writeFileSync(serverFile, serverSrc.replace("<App path={path} />", '<App path={`${path} (edited)`} />'));
  check(
    await until(async () => (await page.evaluate(() => window.__marker)) !== 1 &&
      (await page.textContent(".path")).includes("(edited)")),
    "dev: server-only edit reloads the page with new server HTML",
  );

  // A compile error: shown in Vite's overlay, the old server keeps serving.
  writeFileSync(serverFile, serverSrc.replace("<App path={path} />", "<App path={42} />"));
  check(await until(async () => page.evaluate(() => !!document.querySelector("vite-error-overlay"))), "dev: Velt compile error in Vite's overlay");
  check((await get()).includes("(edited)"), "dev: ... while the previous server keeps serving");
  writeFileSync(serverFile, serverSrc);
  check(
    await until(async () => !(await page.evaluate(() => !!document.querySelector("vite-error-overlay"))) &&
      !(await get()).includes("(edited)")),
    "dev: fixing it clears the overlay and serves the fixed server",
  );
} catch (e) {
  failed++;
  console.log(String(e), "\n--- dev log\n", dev.log());
} finally {
  writeFileSync(counterFile, counterSrc);
  writeFileSync(serverFile, serverSrc);
  dev.kill();
  await browser.close();
}
console.log(failed ? `${failed} failed` : "all passed");
process.exit(failed ? 1 : 0);
