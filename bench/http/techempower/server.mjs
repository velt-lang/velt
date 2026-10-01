// TechEmpower-style benchmark server on plain node:http (same routes and fortunes as
// server.vlt). Usage: node server.mjs <port>. With CLUSTER=1, forks one worker per core
// (node:cluster, shared listening socket).
import http from "node:http";
import cluster from "node:cluster";
import os from "node:os";

const port = Number(process.argv[2] ?? 8080);

function loadFortunes() {
  return [
    { id: 1, message: "fortune: No such file or directory" },
    { id: 2, message: "A computer scientist is someone who fixes things that aren't broken." },
    { id: 3, message: "After enough decimal places, nobody gives a damn." },
    { id: 4, message: "A bad random number generator: 1, 1, 1, 1, 1, 4.33e+67, 1, 1, 1" },
    { id: 5, message: "A computer program does what you tell it to do, not what you want it to do." },
    { id: 6, message: "Emacs is a nice operating system, but I prefer UNIX. — Tom Christaensen" },
    { id: 7, message: "Any program that runs right is obsolete." },
    { id: 8, message: "A list is only as strong as its weakest link. — Donald Knuth" },
    { id: 9, message: "Feature: A bug with seniority." },
    { id: 10, message: "Computers make very fast, very accurate mistakes." },
    { id: 11, message: '<script>alert("This should not be displayed in a browser alert box.");</script>' },
    { id: 12, message: "フレームワークのベンチマーク" },
  ];
}

const escapes = { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" };
const escapeHtml = (s) => s.replace(/[&<>"']/g, (c) => escapes[c]);

function fortunesPage() {
  const fortunes = loadFortunes();
  fortunes.push({ id: 0, message: "Additional fortune added at request time." });
  fortunes.sort((a, b) => (a.message < b.message ? -1 : a.message > b.message ? 1 : 0));
  let html =
    "<!DOCTYPE html><html><head><title>Fortunes</title></head><body><table><tr><th>id</th><th>message</th></tr>";
  for (const f of fortunes) {
    html += `<tr><td>${f.id}</td><td>${escapeHtml(f.message)}</td></tr>`;
  }
  return html + "</table></body></html>";
}

function handle(req, res) {
  if (req.url === "/json") {
    res.writeHead(200, { "content-type": "application/json" });
    res.end(JSON.stringify({ message: "Hello, World!" }));
  } else if (req.url === "/fortunes") {
    res.writeHead(200, { "content-type": "text/html; charset=utf-8" });
    res.end(fortunesPage());
  } else {
    res.writeHead(200, { "content-type": "text/plain; charset=utf-8" });
    res.end("Hello, World!");
  }
}

if (process.env.CLUSTER === "1" && cluster.isPrimary) {
  for (let i = 0; i < os.availableParallelism(); i++) {
    cluster.fork();
  }
  // run.sh stops the server with SIGTERM to the primary: take the workers down with it.
  process.on("SIGTERM", () => {
    for (const w of Object.values(cluster.workers)) w.kill();
    process.exit(0);
  });
} else {
  http.createServer(handle).listen(port, "127.0.0.1", () => {
    if (!cluster.isWorker || cluster.worker.id === 1) console.log(`listening on http://127.0.0.1:${port}`);
  });
}
