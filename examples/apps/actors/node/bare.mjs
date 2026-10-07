// Calibration only: plain node:http answering every POST with {"data":0} after JSON-parsing the
// body, no actors. Tells how much of the sigx numbers is this machine's Node HTTP floor.
//   node bare.mjs [port]
import { createServer } from 'node:http';
createServer((req, res) => {
  let body = '';
  req.on('data', (c) => (body += c));
  req.on('end', () => {
    const { args } = JSON.parse(body);
    res.writeHead(200, { 'content-type': 'application/json' });
    res.end(JSON.stringify({ data: 0, k: args[0] }));
  });
}).listen(Number(process.argv[2] ?? 5399), '127.0.0.1');
