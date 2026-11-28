// Node's twin of fetch_env_proxy.velt: the same servers, proxy and runs, so `node
// fetch_env_proxy.mjs` prints fetch_env_proxy.out (Node 22.21+, which needs NODE_USE_ENV_PROXY=1
// where Velt uses the environment's proxy by default: `run` sets it). The test certificate is read
// from ../_tls.velt; Node's child trusts its CA through NODE_EXTRA_CA_CERTS, Velt's through `ca`.
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
      const res = await fetch(url, { ca: TEST_CA });
      console.log(`${url} ${res.status} ${await res.text()}`);
    } catch (e) {
      console.log(`${url} failed`);
    }
  }
}

async function main() {
  const log = [];
  const plain = http.createServer((req, res) => {
    if (req.url.startsWith("/to-https?")) {
      res.writeHead(302, { location: `https://localhost:${new URL(req.url, "http://x").searchParams.get("to")}/landed` }).end();
      return;
    }
    res.end(`http ${req.headers.host}${req.url}`);
  });
  const secure = https.createServer({ cert: pem("TEST_CERT"), key: pem("TEST_KEY") }, (req, res) => {
    if (req.url.startsWith("/to-http?")) {
      res.writeHead(302, { location: `http://r2.test:${new URL(req.url, "http://x").searchParams.get("to")}/back` }).end();
      return;
    }
    res.end(`https ${req.headers.host}${req.url}`);
  });
  const proxy = http.createServer((req, res) => {
    log.push(`proxy: ${req.method} ${req.url}`);
    res.writeHead(400).end();
  });
  proxy.on("connect", (req, socket, head) => {
    log.push(`proxy: CONNECT ${req.url} auth=${req.headers["proxy-authorization"] ?? "-"}`);
    if (req.url.startsWith("refuse.test:")) {
      socket.end("HTTP/1.1 403 Forbidden\r\ncontent-length: 0\r\n\r\n");
      return;
    }
    const port = Number(req.url.slice(req.url.lastIndexOf(":") + 1));
    // Not 200: undici takes no other answer, 2xx neither, and the tunnel would work.
    const answer = req.url.startsWith("nocontent.test:")
      ? "HTTP/1.1 204 No Content\r\n\r\n"
      : "HTTP/1.1 200 Connection established\r\n\r\n";
    const origin = net.connect(port, "127.0.0.1", () => {
      socket.write(answer);
      origin.write(head);
      origin.pipe(socket);
      socket.pipe(origin);
    });
    origin.on("error", () => socket.destroy());
    socket.on("error", () => origin.destroy());
  });
  for (const server of [plain, secure, proxy]) {
    await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  }

  const P = `http://127.0.0.1:${proxy.address().port}`;
  const H = plain.address().port;
  const S = secure.address().port;
  const caFile = join(mkdtempSync(join(tmpdir(), "fetch-env-proxy-")), "ca.pem");
  writeFileSync(caFile, TEST_CA);

  const hide = (text) =>
    text
      .split(`:${H}`)
      .join(":<http>")
      .split(`:${S}`)
      .join(":<https>")
      .split(`=${H}`)
      .join("=<http>")
      .split(`=${S}`)
      .join("=<https>")
      .split(`:${proxy.address().port}`)
      .join(":<proxy>");

  async function run(name, vars, urls) {
    console.log(`== ${name}`);
    const env = { ...process.env, NODE_EXTRA_CA_CERTS: caFile };
    for (const v of PROXY_VARS) {
      delete env[v];
    }
    for (const k of Object.keys(vars)) {
      // Windows has one variable for both spellings: the lowercase one is set.
      if (process.platform != "win32" || k == k.toLowerCase() || !Object.keys(vars).includes(k.toLowerCase())) {
        env[k] = vars[k];
      }
    }
    // Node uses the environment's proxy only when asked; Velt always does.
    env.NODE_USE_ENV_PROXY = "1";
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

  const both = { http_proxy: P, https_proxy: P };
  await run("by default, for this machine too", both, [
    `http://localhost:${H}/a`,
    `http://a.test:${H}/`,
  ]);
  // Node reads it, Velt does not: `run` sets it to 1 for Node.
  await run("NODE_USE_ENV_PROXY=0 changes nothing", { NODE_USE_ENV_PROXY: "0", http_proxy: P }, [
    `http://b.test:${H}/`,
  ]);
  const withAuth = `http://u:p%40ss@127.0.0.1:${proxy.address().port}`;
  await run("a proxy with credentials", { http_proxy: withAuth, https_proxy: withAuth }, [
    `http://c.test:${H}/c`,
    `http://127.0.0.1:${H}/loopback`,
    `https://localhost:${S}/s`,
  ]);
  // Node sends `https:` through the `http:` proxy when no `https:` one is set; Velt (as Bun)
  // does not, and the `NO_PROXY` here makes Node go direct too.
  await run("http_proxy alone: https: direct", { http_proxy: P, NO_PROXY: "localhost" }, [
    `https://localhost:${S}/s1`,
    `http://s1.test:${H}/`,
  ]);
  await run("a proxy URL with only a user", { http_proxy: `http://u@127.0.0.1:${proxy.address().port}` }, [
    `http://u1.test:${H}/`,
  ]);
  await run("a proxy URL with only a password", { http_proxy: `http://:pw@127.0.0.1:${proxy.address().port}` }, [
    `http://u2.test:${H}/`,
  ]);
  await run("the proxy variables read once, NO_PROXY at each request", { http_proxy: P }, [
    `http://o1.test:${H}/a`,
    "env:http_proxy=",
    `http://o2.test:${H}/b`,
    "env:NO_PROXY=o3.test",
    `http://o3.test:${H}/c`,
  ]);
  await run("uppercase HTTP_PROXY", { HTTP_PROXY: P }, [`http://d.test:${H}/`]);
  await run("HTTPS_PROXY alone", { HTTPS_PROXY: P }, [
    `https://localhost:${S}/e`,
    `http://e.test:${H}/`,
  ]);
  await run("http_proxy over HTTP_PROXY", { http_proxy: P, HTTP_PROXY: "http://127.0.0.1:1" }, [
    `http://f.test:${H}/`,
  ]);
  await run("an empty http_proxy over HTTP_PROXY", { http_proxy: "", HTTP_PROXY: P }, [
    `http://g.test:${H}/`,
  ]);
  await run(
    "NO_PROXY",
    {
      HTTP_PROXY: P,
      NO_PROXY: `n1.test,.n2.test\t*.n3.test n4.test:1 n5.test:${H} [::1] ::5 N6.TEST`,
    },
    [
      `http://n1.test:${H}/`,
      `http://x.n1.test:${H}/`,
      `http://n2.test:${H}/`,
      `http://x.n2.test:${H}/`,
      `http://x.n3.test:${H}/`,
      `http://n4.test:${H}/`,
      `http://n5.test:${H}/`,
      `http://[::1]:${H}/`,
      `http://[::5]:${H}/`,
      `http://n6.test:${H}/`,
    ],
  );
  await run("no_proxy over NO_PROXY, even empty", { HTTP_PROXY: P, no_proxy: "", NO_PROXY: "h.test" }, [
    `http://h.test:${H}/`,
  ]);
  await run("NO_PROXY=*", { HTTP_PROXY: P, NO_PROXY: "*" }, [`http://i.test:${H}/`]);
  await run("redirects between http: and https:", { HTTP_PROXY: P, HTTPS_PROXY: P, NO_PROXY: "localhost" }, [
    `http://r1.test:${H}/to-https?to=${S}`,
    `https://127.0.0.1:${S}/to-http?to=${H}`,
  ]);
  await run("a proxy that refuses", { HTTP_PROXY: P }, [
    `http://refuse.test:${H}/`,
  ]);
  await run("a proxy that answers 204", { HTTP_PROXY: P }, [
    `http://nocontent.test:${H}/`,
  ]);
  await run("no proxy there", { HTTP_PROXY: "http://127.0.0.1:1" }, [
    `http://k.test:${H}/`,
  ]);
  for (const server of [plain, secure, proxy]) {
    server.close();
    server.closeAllConnections?.();
  }
}

if (process.argv.length > 2 && process.argv[2] == "child") {
  await child(process.argv.slice(3));
  // Done: a pooled TLS connection would keep the program waiting for a while.
  process.exit(0);
} else {
  await main();
}
