// End-to-end check of the sigx + Velt packaging POC. Needs `pnpm install`, `velt` on PATH (or
// $VELT) and Playwright's Chromium. Run `pnpm build` first. Steps:
//   1. bytes:   Velt's streamed document for each page equals JavaScript sigx's
//               (scripts/reference.mjs --stream); the shell does not wait for data; a server
//               function answers in sigx's wire format
//   2. prod:    dist/server/app serves the client build; the page hydrates with the streamed
//               data restored (no server-function call), clicks work, no console warnings;
//               links navigate without a page load, back works
//   3. dev/HMR: under `vite`, a shared component edit hot-updates the browser (state kept) and
//               the server; a server-only edit reloads the page; a Velt compile error shows in
//               Vite's overlay while the old server keeps serving.
import { spawn, execFileSync } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import { createServer } from "node:net";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright";

const root = fileURLToPath(new URL("..", import.meta.url));
const counterFile = `${root}/src/shared/Counter.tsx`;
const serverFile = `${root}/src/server.vlt`;
const counterSrc = readFileSync(counterFile, "utf8");
const serverSrc = readFileSync(serverFile, "utf8");
const PAGES = ["/", "/about", "/islands"];
// sigx's islands also record each island's named signals ("state"), which needs its Vite
// transform's names; Velt's server leaves them out and the browser re-runs setup from the props.
const withoutState = (html) => html.replace(/,"state":\{[^{}]*\}/g, "");
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

// The client sets data-hydrated once hydrate() has finished: clicks before that are lost.
const hydrated = (page) => page.waitForSelector("html[data-hydrated]", { state: "attached", timeout: 10000 });

// --- 1 + 2: production binary ---
const port = await freePort();
const prod = await start("./dist/server/app", ["--port", String(port)], /listening/);
let browser;
try {
  browser = await chromium.launch();
  // The streamed document, whole: shell, $SIGX_REPLACE scripts, state, completion.
  for (const path of PAGES) {
    const ref = execFileSync("node", ["scripts/reference.mjs", "--stream", path], { cwd: root, encoding: "utf8" });
    const devPort = await freePort();
    const raw = await start("./dist/server/app", ["--port", String(devPort), "--template", "index.html", "--dev"], /listening/);
    const html = await (await fetch(`http://127.0.0.1:${devPort}${path}`)).text();
    raw.kill();
    check(html === withoutState(ref), `stream: Velt's streamed ${path} equals JavaScript sigx's renderDocumentToWebStream`);
  }
  // The shell does not wait for the data.
  {
    const t0 = Date.now();
    const res = await fetch(`http://127.0.0.1:${port}/`);
    const reader = res.body.getReader();
    const first = new TextDecoder().decode((await reader.read()).value);
    const shellMs = Date.now() - t0;
    let rest = "";
    for (let r = await reader.read(); !r.done; r = await reader.read()) rest += new TextDecoder().decode(r.value);
    const allMs = Date.now() - t0;
    check(first.includes("Loading…") && allMs - shellMs >= 200 && rest.includes("$SIGX_REPLACE"),
      `stream: shell after ${shellMs} ms with the pending state, data after ${allMs} ms`);
  }
  const fn = await fetch(`http://127.0.0.1:${port}/_sigx/fn/src/api.server/greet`, {
    method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ args: ["e2e"] }),
  });
  check(fn.status === 200 && (await fn.text()) === '{"data":"Hello, e2e, from a Velt server function"}', "server function: POST /_sigx/fn answers in sigx's format");

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
  const fnCalls = [];
  page.on("request", (r) => r.url().includes("/_sigx/fn/") && fnCalls.push(r.url()));
  await page.goto(`http://127.0.0.1:${port}/`, { waitUntil: "networkidle" });
  await hydrated(page);
  check((await page.textContent(".stats")).includes("Rendered by Velt (native)"), "prod: streamed data is on the page");
  check(fnCalls.length === 0, `prod: hydration restores the data without calling the server${fnCalls.length ? `: ${fnCalls}` : ""}`);
  await page.click("#inc");
  await page.click("#inc");
  check(
    await until(async () => (await page.textContent(".card p")).startsWith("Count: 3 (doubled 6)")),
    "prod: hydrated, two clicks give Count: 3",
  );
  check(messages.length === 0, `prod: no console warnings or errors${messages.length ? `: ${messages.join(" | ")}` : ""}`);

  // Islands: only the islands hydrate (the app's code is never loaded), each works.
  {
    const isl = await browser.newPage();
    const islMessages = [];
    isl.on("console", (m) => (m.type() === "warning" || m.type() === "error") && islMessages.push(m.text()));
    const islRequests = [];
    isl.on("request", (r) => islRequests.push(new URL(r.url()).pathname));
    await isl.goto(`http://127.0.0.1:${port}/islands`, { waitUntil: "networkidle" });
    await hydrated(isl);
    for (const b of await isl.$$("p.clicker button")) await b.click();
    check(
      await until(async () => (await isl.$$eval("p.clicker", (ps) => ps.map((p) => p.textContent).join("|"))) === "load count: 2|visible count: 6|only count: 10"),
      "islands: load, visible and only islands hydrate and count",
    );
    check(!islRequests.some((r) => r.includes("/App-")), "islands: the app's code is not loaded");
    check(
      (await isl.$$eval('link[rel=modulepreload]', (ls) => ls.map((l) => l.getAttribute("href")))).some((h) => /\/assets\/Clicker-/.test(h)),
      "islands: the server preloads the island's chunk (from the build's islands manifest)",
    );
    check(islMessages.length === 0, `islands: no console warnings or errors${islMessages.length ? `: ${islMessages}` : ""}`);
    check((await fetch(`http://127.0.0.1:${port}/.vite/sigx-islands-manifest.json`)).headers.get("content-type").startsWith("text/html"), "islands: the build's manifests are not served");
    await isl.close();
  }

  // The router: a link click navigates in the browser (no page load), the back button returns.
  await page.evaluate(() => (window.__marker = 1));
  await page.click('a[href="/about"]');
  check(
    await until(async () => page.url().endsWith("/about") && (await page.textContent("section h2")) === "About"),
    "prod: a link click shows /about",
  );
  check(await page.evaluate(() => window.__marker === 1), "prod: ... without a page load");
  check((await page.getAttribute('a[href="/about"]', "class")) === "active", "prod: ... and marks its link active");
  await page.goBack();
  check(await until(async () => (await page.textContent(".card p")).startsWith("Count:")), "prod: the back button shows / again");
  const direct = await browser.newPage();
  await direct.goto(`http://127.0.0.1:${port}/about`, { waitUntil: "networkidle" });
  await hydrated(direct);
  await direct.click('a[href="/"]');
  check(await until(async () => (await direct.textContent(".card p")).startsWith("Count: 1")), "prod: /about loaded directly hydrates and navigates to /");
  await direct.close();
  await page.close();
} finally {
  prod.kill();
}

// --- 3: dev server and HMR ---
// The dev steps edit these; they are put back on any exit, Ctrl-C included.
process.on("SIGINT", () => process.exit(130));
process.on("exit", () => {
  writeFileSync(counterFile, counterSrc);
  writeFileSync(serverFile, serverSrc);
});
const devPort = await freePort();
const dev = await start("./node_modules/.bin/vite", ["--port", String(devPort), "--strictPort"], /sigx: listening/);
const url = `http://localhost:${devPort}/`;
const get = async () => (await fetch(url, { headers: { accept: "text/html" } })).text();
try {
  const html = await get();
  check(html.includes("/@vite/client") && html.includes("<!--$c:1-->"), "dev: Velt renders the page, Vite injects its client");

  const page = await browser.newPage();
  await page.goto(url, { waitUntil: "networkidle" });
  await hydrated(page);
  await page.click("#inc");
  await page.evaluate(() => (window.__marker = 1)); // gone after a full reload
  check(await until(async () => (await page.textContent(".card p")).startsWith("Count: 2")), "dev: hydrated");

  // A shared component: Vite's HMR updates the browser in place, velt dev the server.
  writeFileSync(counterFile, counterSrc.replace(/>\s*\+1\s*</, ">add one<"));
  check(await until(async () => (await page.textContent("#inc")) === "add one"), "dev: shared edit hot-updates the browser");
  check(await page.evaluate(() => window.__marker === 1), "dev: ... without a full reload");
  check(await until(async () => (await get()).includes(">add one</button>")), "dev: ... and the server renders the new markup");

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
  check(await until(async () => (await get()).includes("(edited)")), "dev: ... while the previous server keeps serving");
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
