// Cumulus listen-along relay. One Durable Object "room" per listening
// session: the host's Cumulus sends what it's playing (track id, position,
// playing/paused), and the room passes that on to everyone listening along.
// No audio goes through here, and nothing is kept once the session ends.

import { DurableObject } from "cloudflare:workers";

/** Most people listening along to one host. */
const MAX_LISTENERS = 20;
/** Largest message a host may send. */
const MAX_MESSAGE = 1024;
/** Host messages allowed per 10 seconds (state changes plus heartbeats). */
const HOST_BURST = 30;
/** How long a room waits for its host to reconnect before ending. */
const HOST_GRACE_MS = 60_000;
/** Rooms end after this long, whatever happens. */
const MAX_ROOM_MS = 12 * 60 * 60_000;

const HEX32 = /^[0-9a-f]{32}$/;

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    if (url.pathname === "/") {
      return new Response("Cumulus listen-along relay\n");
    }
    const match = url.pathname.match(/^\/v1\/rooms\/([0-9a-f]{32})$/);
    if (!match) {
      return new Response("not found\n", { status: 404 });
    }
    if (request.headers.get("Upgrade") !== "websocket") {
      return new Response("expected a WebSocket\n", { status: 426 });
    }
    const role = url.searchParams.get("role");
    if (role !== "host" && role !== "listen") {
      return new Response("bad role\n", { status: 400 });
    }
    if (role === "host" && !HEX32.test(url.searchParams.get("key") ?? "")) {
      return new Response("bad key\n", { status: 400 });
    }
    const room = env.ROOMS.get(env.ROOMS.idFromName(match[1]));
    return room.fetch(request);
  },
};

async function sha256(text) {
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text));
  return [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, "0")).join("");
}

export class Room extends DurableObject {
  async fetch(request) {
    const url = new URL(request.url);
    const role = url.searchParams.get("role");
    const host = await this.ctx.storage.get("host");

    if (role === "host") {
      const hash = await sha256(url.searchParams.get("key"));
      // The first host to connect owns the room; only the same key can reconnect.
      if (host && host !== hash) {
        return new Response("wrong key\n", { status: 403 });
      }
      if (!host) {
        await this.ctx.storage.put("host", hash);
        await this.ctx.storage.put("expires", Date.now() + MAX_ROOM_MS);
      }
      const [client, server] = Object.values(new WebSocketPair());
      for (const old of this.ctx.getWebSockets("host")) {
        old.close(4000, "replaced");
      }
      this.ctx.acceptWebSocket(server, ["host"]);
      server.serializeAttachment({ window: Date.now(), count: 0 });
      await this.ctx.storage.setAlarm(await this.ctx.storage.get("expires"));
      this.sendCount(server);
      return new Response(null, { status: 101, webSocket: client });
    }

    if (!host) {
      return new Response("no such session\n", { status: 404 });
    }
    if (this.listeners().length >= MAX_LISTENERS) {
      return new Response("session is full\n", { status: 409 });
    }
    const [client, server] = Object.values(new WebSocketPair());
    this.ctx.acceptWebSocket(server, ["listen"]);
    // A late joiner gets the last state, with how old it is (a playing
    // host has moved on since).
    const state = await this.ctx.storage.get("state");
    if (state) {
      const age = Math.max(0, Date.now() - ((await this.ctx.storage.get("stateAt")) ?? Date.now()));
      server.send(JSON.stringify({ ...JSON.parse(state), age }));
    }
    this.sendCount();
    return new Response(null, { status: 101, webSocket: client });
  }

  open(tag) {
    return this.ctx.getWebSockets(tag).filter((ws) => ws.readyState === WebSocket.OPEN);
  }

  listeners() {
    return this.open("listen");
  }

  /** Tells the host how many people are listening along. */
  sendCount(to) {
    const message = JSON.stringify({ type: "listeners", count: this.listeners().length });
    for (const ws of to ? [to] : this.open("host")) {
      try {
        ws.send(message);
      } catch {}
    }
  }

  async webSocketMessage(ws, message) {
    // Listeners only listen.
    if (!this.ctx.getTags(ws).includes("host")) {
      return;
    }
    if (typeof message !== "string" || message.length > MAX_MESSAGE) {
      return;
    }
    const rate = ws.deserializeAttachment() ?? { window: 0, count: 0 };
    const now = Date.now();
    if (now - rate.window > 10_000) {
      rate.window = now;
      rate.count = 0;
    }
    rate.count += 1;
    ws.serializeAttachment(rate);
    if (rate.count > HOST_BURST) {
      return;
    }
    let m;
    try {
      m = JSON.parse(message);
    } catch {
      return;
    }
    if (m?.type !== "state") {
      return;
    }
    // Pass on only these fields, checked, whatever the host sent.
    const track = Number.isSafeInteger(m.track) && m.track > 0 ? m.track : null;
    const position = Number.isFinite(m.position) ? Math.max(0, Math.min(m.position, 86_400)) : 0;
    const out = JSON.stringify({ type: "state", track, position, playing: m.playing === true });
    await this.ctx.storage.put({ state: out, stateAt: Date.now() });
    for (const listener of this.listeners()) {
      try {
        listener.send(out);
      } catch {}
    }
  }

  async webSocketClose(ws, code) {
    await this.left(ws, code);
  }

  async webSocketError(ws) {
    await this.left(ws, 1011);
  }

  async left(ws, code) {
    try {
      ws.close(code === 1005 ? 1000 : code, "bye");
    } catch {}
    if (!this.ctx.getTags(ws).includes("host")) {
      this.sendCount();
      return;
    }
    if (code === 1000) {
      // The host ended the session.
      return this.end();
    }
    // The host dropped: give it a minute to come back.
    if (this.open("host").length === 0) {
      await this.ctx.storage.setAlarm(Date.now() + HOST_GRACE_MS);
    }
  }

  async alarm() {
    const expires = (await this.ctx.storage.get("expires")) ?? 0;
    if (this.open("host").length > 0 && Date.now() < expires) {
      await this.ctx.storage.setAlarm(expires);
      return;
    }
    await this.end();
  }

  /** Tells everyone the session is over and forgets the room. */
  async end() {
    const message = JSON.stringify({ type: "ended" });
    for (const ws of this.ctx.getWebSockets()) {
      try {
        ws.send(message);
        ws.close(1000, "session ended");
      } catch {}
    }
    await this.ctx.storage.deleteAlarm();
    await this.ctx.storage.deleteAll();
  }
}
