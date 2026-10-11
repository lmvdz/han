// Frame tap: a transparent WebSocket proxy that logs every text frame in both directions as JSON lines.
//   node tap.mjs --listen PORT --upstream PORT --log FILE [--upstream-path /han]
// The client's request path is forwarded unchanged unless --upstream-path is given (so a path mismatch shows up as an
// `upgrade-fail` event). `GET /__mark?name=X` writes a marker line, so a driver can delimit its operations in the log.
import http from "node:http";
import fs from "node:fs";
import { createRequire } from "node:module";
const require = createRequire(import.meta.url);
const WebSocket = require("../cleanroom-ts/node_modules/ws");
const { WebSocketServer } = WebSocket;

const arg = (n, d) => { const i = process.argv.indexOf("--" + n); return i < 0 ? d : process.argv[i + 1]; };
const LISTEN = Number(arg("listen")), UP = Number(arg("upstream")), LOG = arg("log"), UPATH = arg("upstream-path", null);
const fd = fs.openSync(LOG, "a");
let n = 0, connN = 0;
const log = (o) => fs.writeSync(fd, JSON.stringify({ n: n++, ms: Date.now(), ...o }) + "\n");

const server = http.createServer((req, res) => {
  const u = new URL(req.url, "http://x");
  if (u.pathname === "/__mark") { log({ ev: "mark", name: u.searchParams.get("name") }); res.writeHead(200); return res.end("ok"); }
  // plain HTTP (the two .well-known documents) is passed through untouched
  const up = http.request({ host: "127.0.0.1", port: UP, path: req.url, method: req.method, headers: req.headers }, (r) => { res.writeHead(r.statusCode, r.headers); r.pipe(res); });
  up.on("error", () => { res.writeHead(502); res.end(); });
  req.pipe(up);
});
const wss = new WebSocketServer({ noServer: true });
server.on("upgrade", (req, socket, head) => {
  const conn = connN++, path = UPATH ?? req.url;
  const up = new WebSocket(`ws://127.0.0.1:${UP}${path}`, { perMessageDeflate: false });
  const toClient = [];
  let client = null;
  log({ ev: "open", conn, path: req.url, upstreamPath: path });
  up.on("unexpected-response", (_rq, res) => {
    log({ ev: "upgrade-fail", conn, status: res.statusCode, path });
    socket.write(`HTTP/1.1 ${res.statusCode} ${res.statusMessage}\r\nConnection: close\r\n\r\n`); socket.destroy();
  });
  up.on("error", (e) => { log({ ev: "upstream-error", conn, message: String(e.message) }); });
  up.on("message", (d, bin) => {
    if (bin) { log({ ev: "binary", conn, dir: "s2c", bytes: d.length }); }
    else log({ ev: "frame", conn, dir: "s2c", data: d.toString("utf8") });
    if (client) client.send(d, { binary: bin }); else toClient.push([d, bin]);
  });
  up.on("close", (code) => { log({ ev: "close", conn, by: "server", code }); if (client) client.close(); else socket.destroy(); });
  up.on("open", () => {
    wss.handleUpgrade(req, socket, head, (ws) => {
      client = ws;
      for (const [d, bin] of toClient.splice(0)) ws.send(d, { binary: bin });
      ws.on("message", (d, bin) => {
        if (bin) log({ ev: "binary", conn, dir: "c2s", bytes: d.length }); else log({ ev: "frame", conn, dir: "c2s", data: d.toString("utf8") });
        if (up.readyState === WebSocket.OPEN) up.send(d, { binary: bin });
      });
      ws.on("close", (code) => { log({ ev: "close", conn, by: "client", code }); try { up.close(); } catch { /* */ } });
      ws.on("error", () => { /* closed by the close handler */ });
    });
  });
});
server.listen(LISTEN, "127.0.0.1", () => console.log(`TAP listening ${LISTEN} -> ${UP}`));
