// Bun's twin of fetch_env_proxy_bun.velt: the same servers, proxies and runs, so `bun
// fetch_env_proxy_bun.mjs` prints fetch_env_proxy_bun.out. Bun sends `http:` to a proxy as a
// request for the absolute URL, which these proxies forward, where Velt tunnels it with
// `CONNECT`: the log names only the host asked for. The test certificate is read from
// ../_tls.velt; the child trusts its CA through `tls.ca` and NODE_EXTRA_CA_CERTS (the `https://`
// proxy's certificate too).
import { spawn } from "node:child_process";
import { mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import http from "node:http";
import https from "node:https";
import net from "node:net";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const tlsSource = readFileSync(join(here, "..", "_tls.velt"), "utf8");
const pem = (name) => JSON.parse(tlsSource.match(new RegExp(`${name} =\\s*("[^"]*")`))[1]);
const TEST_CA = pem("TEST_CA");

const PROXY_VARS = [
  "NODE_USE_ENV_PROXY",
  "http_proxy",
  "HTTP_PROXY",
  "https_proxy",
  "HTTPS_PROXY",
  "all_proxy",
  "ALL_PROXY",
  "no_proxy",
  "NO_PROXY",
];

async function child(urls) {
  for (const url of urls) {
    // `env:NAME=VALUE` sets a variable between two requests.
    if (url.startsWith("env:")) {
      const eq = url.indexOf("=");
      process.env[url.slice(4, eq)] = url.slice(eq + 1);
      console.log(url);
      continue;
    }
    try {
      const res = await fetch(url, { tls: { ca: TEST_CA } });
      console.log(`${url} ${res.status} ${await res.text()}`);
    } catch (e) {
      console.log(`${url} failed`);
    }
  }
}

/** A proxy (`server`: an http or https server) logging as `name` what it is asked for. */
function proxying(server, log, name) {
  // An absolute URL: forwarded to the origin on this machine.
  server.on("request", (req, res) => {
    const target = new URL(req.url);
    log.push(`${name}: ${target.host}`);
    const out = http.request(
      { host: "127.0.0.1", port: target.port, path: target.pathname + target.search, method: req.method, headers: req.headers },
      (answer) => {
        res.writeHead(answer.statusCode, answer.headers);
        answer.pipe(res);
      },
    );
    out.on("error", () => res.writeHead(502).end());
    req.pipe(out);
  });
  server.on("connect", (req, socket, head) => {
    log.push(`${name}: ${req.url}`);
    const port = Number(req.url.slice(req.url.lastIndexOf(":") + 1));
    const origin = net.connect(port, "127.0.0.1", () => {
      socket.write("HTTP/1.1 200 Connection established\r\n\r\n");
      origin.write(head);
      origin.pipe(socket);
      socket.pipe(origin);
    });
    origin.on("error", () => socket.destroy());
    socket.on("error", () => origin.destroy());
  });
  return server;
}

async function main() {
  const log = [];
  const plain = http.createServer((req, res) => {
    res.end(`http ${req.headers.host}${req.url}`);
  });
  const secure = https.createServer({ cert: pem("TEST_CERT"), key: pem("TEST_KEY") }, (req, res) => {
    res.end(`https ${req.headers.host}${req.url}`);
  });
  const proxy = proxying(http.createServer(), log, "proxy");
  const tlsProxy = proxying(https.createServer({ cert: pem("TEST_CERT"), key: pem("TEST_KEY") }), log, "tls proxy");
  for (const server of [plain, secure, proxy, tlsProxy]) {
    await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  }

  const P = `http://127.0.0.1:${proxy.address().port}`;
  const TP = `https://localhost:${tlsProxy.address().port}`;
  const H = plain.address().port;
  const S = secure.address().port;
  const caFile = join(mkdtempSync(join(tmpdir(), "fetch-env-proxy-bun-")), "ca.pem");
  writeFileSync(caFile, TEST_CA);

  const hide = (text) =>
    text
      .split(`:${H}`)
      .join(":<http>")
      .split(`:${S}`)
      .join(":<https>")
      .split(`:${proxy.address().port}`)
      .join(":<proxy>")
      .split(`:${tlsProxy.address().port}`)
      .join(":<tls proxy>");

  async function run(name, vars, urls) {
    console.log(`== ${name}`);
    const env = { ...process.env, NODE_EXTRA_CA_CERTS: caFile };
    for (const v of PROXY_VARS) {
      delete env[v];
    }
    for (const k of Object.keys(vars)) {
      // Windows has one variable for both spellings: the lowercase one is set, or the uppercase
      // one when the lowercase is empty.
      const lower = k.toLowerCase();
      const pair = k == lower ? k.toUpperCase() : lower;
      const keep =
        process.platform != "win32" ||
        !Object.keys(vars).includes(pair) ||
        (k == lower ? vars[k] != "" : vars[lower] == "");
      if (keep) {
        env[k] = vars[k];
      }
    }
    const out = await new Promise((resolve) => {
      const c = spawn(process.execPath, [process.argv[1], "child", ...urls], {
        env,
        stdio: ["ignore", "pipe", "ignore"],
      });
      let text = "";
      c.stdout.on("data", (d) => (text += d));
      c.on("close", () => resolve(text));
    });
    console.log(hide(out.trimEnd()));
    for (const line of log.splice(0)) {
      console.log(hide(line));
    }
  }

  await run("an empty http_proxy reads HTTP_PROXY", { http_proxy: "", HTTP_PROXY: P }, [
    `http://g.test:${H}/`,
  ]);
  await run("'' is empty; ALL_PROXY when the scheme's is unset", { http_proxy: "''", ALL_PROXY: P }, [
    `http://al.test:${H}/`,
    `https://localhost:${S}/al`,
  ]);
  await run("ALL_PROXY naming a SOCKS proxy is ignored", { ALL_PROXY: "socks5://127.0.0.1:1" }, [
    `http://localhost:${H}/so`,
  ]);
  await run('a lowercase "" is no proxy', { http_proxy: '""', HTTP_PROXY: P }, [`http://127.0.0.1:${H}/g5`]);
  await run("an https:// proxy", { http_proxy: TP, https_proxy: TP }, [
    `http://t1.test:${H}/`,
    `https://localhost:${S}/t2`,
    `https://127.0.0.1:${S}/t3`,
  ]);
  await run("an https:// ALL_PROXY", { ALL_PROXY: TP }, [`http://t4.test:${H}/`]);
  await run("the variables read at each request", { http_proxy: P }, [
    `http://o1.test:${H}/a`,
    "env:http_proxy=",
    `http://o2.test:${H}/b`,
    `env:http_proxy=${P}`,
    "env:NO_PROXY=o3.test",
    `http://o3.test:${H}/c`,
    `http://o4.test:${H}/d`,
  ]);
  await run("NO_PROXY", { HTTP_PROXY: P, NO_PROXY: "n1.test,.n2.test ::1 127.0.0.0/8 n3.test." }, [
    `http://x.n1.test:${H}/`,
    `http://n2.test:${H}/`,
    `http://xn1.test:${H}/`,
    `http://[::1]:${H}/`,
    `http://127.0.0.1:${H}/block`,
    `http://n3.test:${H}/`,
  ]);
  await run("an empty no_proxy reads NO_PROXY", { HTTP_PROXY: P, no_proxy: "", NO_PROXY: "h.test" }, [
    `http://h.test:${H}/`,
  ]);
  for (const server of [plain, secure, proxy, tlsProxy]) {
    server.close();
    server.closeAllConnections?.();
  }
}

if (process.argv.length > 2 && process.argv[2] == "child") {
  await child(process.argv.slice(3));
  process.exit(0);
} else {
  await main();
}
