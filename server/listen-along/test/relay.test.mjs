// End-to-end checks against a running relay (`npm run dev` first, or set
// RELAY to another address). Uses Node's built-in WebSocket.
import assert from "node:assert/strict";
import { randomBytes } from "node:crypto";

const RELAY = process.env.RELAY ?? "ws://127.0.0.1:8787";
const hex = () => randomBytes(16).toString("hex");

/** Opens a socket; resolves once open, rejects if the server refuses it. */
function connect(room, role, key) {
  const q = role === "host" ? `role=host&key=${key}` : "role=listen";
  const ws = new WebSocket(`${RELAY}/v1/rooms/${room}?${q}`);
  ws.inbox = [];
  ws.waiters = [];
  ws.closed = new Promise((resolve) => ws.addEventListener("close", (e) => resolve(e.code)));
  ws.addEventListener("message", (e) => {
    const m = JSON.parse(e.data);
    const w = ws.waiters.shift();
    w ? w(m) : ws.inbox.push(m);
  });
  return new Promise((resolve, reject) => {
    ws.addEventListener("open", () => resolve(ws));
    ws.addEventListener("error", () => reject(new Error("refused")));
  });
}

/** The next message, or fails after `ms`. */
function next(ws, ms = 3000, what = "") {
  if (ws.inbox.length) return Promise.resolve(ws.inbox.shift());
  return new Promise((resolve, reject) => {
    const waiter = (m) => {
      clearTimeout(t);
      resolve(m);
    };
    const t = setTimeout(() => {
      ws.waiters.splice(ws.waiters.indexOf(waiter), 1);
      reject(new Error(`no message ${what}`));
    }, ms);
    ws.waiters.push(waiter);
  });
}

/** True if nothing arrives within `ms`. */
async function quiet(ws, ms = 600) {
  try {
    await next(ws, ms);
    return false;
  } catch {
    return true;
  }
}

const tests = {
  async "listening to a room with no host is refused"() {
    await assert.rejects(connect(hex(), "listen"));
  },

  async "state is relayed, cleaned up, and sent to late joiners"() {
    const room = hex();
    const key = hex();
    const host = await connect(room, "host", key);
    assert.deepEqual(await next(host, 3000, "line 58"), { type: "listeners", count: 0 });

    const a = await connect(room, "listen");
    assert.deepEqual(await next(host, 3000, "line 61"), { type: "listeners", count: 1 });

    host.send(JSON.stringify({ type: "state", track: 123, position: 61.5, playing: true, secret: "x" }));
    assert.deepEqual(await next(a, 3000, "line 64"), { type: "state", track: 123, position: 61.5, playing: true, requests: false });

    await new Promise((r) => setTimeout(r, 300));
    const b = await connect(room, "listen");
    const late = await next(b);
    assert.ok(late.age >= 250 && late.age < 3000, `age ${late.age}`);
    delete late.age;
    assert.deepEqual(late, { type: "state", track: 123, position: 61.5, playing: true, requests: false });
    assert.deepEqual(await next(host, 3000, "line 68"), { type: "listeners", count: 2 });

    // Listeners can't send anything on.
    b.send(JSON.stringify({ type: "state", track: 999, position: 0, playing: false }));
    assert.ok(await quiet(a));

    // Junk and oversized messages are dropped.
    host.send("not json");
    host.send(JSON.stringify({ type: "state", track: 5, pad: "x".repeat(2000) }));
    assert.ok(await quiet(a));

    // Bad values are clamped.
    host.send(JSON.stringify({ type: "state", track: -4, position: 1e9, playing: "yes" }));
    assert.deepEqual(await next(a, 3000, "line 81"), { type: "state", track: null, position: 86400, playing: false, requests: false });
    await next(b);

    a.close();
    assert.deepEqual(await next(host, 3000, "(count after a listener left)"), { type: "listeners", count: 1 });

    // Ending the session tells listeners and forgets the room.
    host.close(1000);
    assert.deepEqual(await next(b, 3000, "(ended)"), { type: "ended" });
    assert.equal(await b.closed, 1000);
    await assert.rejects(connect(room, "listen"));
  },

  async "only the room's own key can host it"() {
    const room = hex();
    const key = hex();
    const host = await connect(room, "host", key);
    await next(host, 3000, "line 97");
    await assert.rejects(connect(room, "host", hex()));
    // The same key can reconnect (e.g. after a network blip).
    const again = await connect(room, "host", key);
    assert.equal(await host.closed, 4000);
    await next(again, 3000, "line 102");
    again.close(1000);
  },

  async "a host that drops can come back to the same room"() {
    const room = hex();
    const key = hex();
    const host = await connect(room, "host", key);
    await next(host);
    const l = await connect(room, "listen");
    await next(host);
    host.send(JSON.stringify({ type: "state", track: 7, position: 10, playing: true }));
    await next(l);
    host.close(4001); // not "ended": e.g. the network dropped
    assert.ok(await quiet(l, 1000));
    const back = await connect(room, "host", key);
    assert.deepEqual(await next(back), { type: "listeners", count: 1 });
    // Late joiners still get the last state.
    const late = await connect(room, "listen");
    const m = await next(late);
    delete m.age;
    assert.deepEqual(m, { type: "state", track: 7, position: 10, playing: true, requests: false });
    back.close(1000);
  },

  async "song requests reach the host only when allowed, and replies reach the asker"() {
    const room = hex();
    const key = hex();
    const host = await connect(room, "host", key);
    await next(host);
    const a = await connect(room, "listen");
    await next(host);
    const b = await connect(room, "listen");
    await next(host);

    // Closed until the host allows requests.
    host.send(JSON.stringify({ type: "state", track: 1, position: 0, playing: true, requests: false }));
    await next(a);
    await next(b);
    a.send(JSON.stringify({ type: "request", track: 55, name: "Al" }));
    assert.deepEqual(await next(a), { type: "reply", track: 55, status: "closed" });
    assert.ok(await quiet(host));

    host.send(JSON.stringify({ type: "state", track: 1, position: 0, playing: true, requests: true }));
    assert.equal((await next(a)).requests, true);
    await next(b);
    a.send(JSON.stringify({ type: "request", track: 55, name: "Al " + "x".repeat(60) }));
    const req = await next(host);
    assert.equal(req.type, "request");
    assert.equal(req.track, 55);
    assert.equal(req.name.length, 40);
    assert.ok(!req.name.includes(""));
    assert.match(req.from, /^[0-9a-f]{8}$/);

    // The answer goes only to the listener who asked.
    host.send(JSON.stringify({ type: "reply", to: req.from, track: 55, status: "added" }));
    assert.deepEqual(await next(a), { type: "reply", track: 55, status: "added" });
    assert.ok(await quiet(b));
    // Made-up statuses are dropped.
    host.send(JSON.stringify({ type: "reply", to: req.from, track: 55, status: "<script>" }));
    assert.ok(await quiet(a));

    // Bad requests are ignored.
    b.send(JSON.stringify({ type: "request", track: -1 }));
    b.send(JSON.stringify({ type: "state", track: 9, position: 0, playing: true }));
    assert.ok(await quiet(host));

    // At most 10 a minute per listener.
    for (let i = 0; i < 12; i++) b.send(JSON.stringify({ type: "request", track: 100 + i, name: "B" }));
    let forwarded = 0;
    while (!(await quiet(host, 500))) forwarded++;
    assert.equal(forwarded, 8); // its 2 earlier (ignored) messages count too
    assert.deepEqual(await next(b), { type: "reply", track: 108, status: "limit" });
    host.close(1000);
  },

  async "hosts can't flood the room"() {
    const room = hex();
    const key = hex();
    const host = await connect(room, "host", key);
    await next(host, 3000, "line 110");
    const l = await connect(room, "listen");
    await next(host, 3000, "line 112");
    for (let i = 0; i < 40; i++) {
      host.send(JSON.stringify({ type: "state", track: 1, position: i, playing: true }));
    }
    let got = 0;
    while (!(await quiet(l, 500))) got++;
    assert.equal(got, 30);
    host.close(1000);
  },
};

let failed = 0;
for (const [name, run] of Object.entries(tests)) {
  try {
    await run();
    console.log(`ok    ${name}`);
  } catch (e) {
    failed++;
    console.log(`FAIL  ${name}\n      ${e.stack ?? e}`);
  }
}
console.log(failed ? `${failed} failed` : "all passed");
process.exit(failed ? 1 : 0);
