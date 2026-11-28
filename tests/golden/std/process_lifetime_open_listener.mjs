// Node twin of process_lifetime_open_listener.velt (velt/net's listener as `net.Server`).
import net from "node:net";

const server = net.createServer();
const accepted = new Promise((resolve) => server.once("connection", resolve));
server.listen(0, "127.0.0.1", () => {
  setTimeout(async () => {
    console.log("unref'd timeout runs: the listener keeps the process alive");
    const c = net.connect(server.address().port, "127.0.0.1");
    const s = await accepted;
    s.end("hi");
    const text = await new Promise((resolve) => {
      let got = "";
      c.on("data", (d) => (got += d));
      c.on("end", () => resolve(got));
    });
    console.log("read:", text);
    c.end();
    server.close();
    console.log("listener closed");
  }, 20).unref();
  setInterval(() => console.log("never: an unref'd interval"), 3600000).unref();
  console.log("main done");
});
