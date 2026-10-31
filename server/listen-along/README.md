# Cumulus listen-along relay

A Cloudflare Worker with one Durable Object per listening session. The
host's Cumulus sends what it's playing (track id, position, playing or
paused); the relay passes that on to everyone listening along. No audio goes
through it.

- `wss://<worker>/v1/rooms/<room>?role=host&key=<key>`: the first host to
  connect owns the room (only a hash of its key is kept).
- `wss://<worker>/v1/rooms/<room>?role=listen`: follow it. Late joiners get
  the latest state, with its age in ms.
- Limits: 20 listeners per room, 1 KB per message, 30 host messages per
  10 s. Only `track`, `position` and `playing` are passed on.
- Nothing is kept after a session: the host ending it (or not coming back
  within a minute of dropping, or 12 hours passing) deletes the room.

## Deploy

Needs a (free) Cloudflare account and Node.js.

```bash
npm install
npx wrangler login
npx wrangler deploy
```

`wrangler login` opens Cloudflare in your browser to sign in. `deploy`
prints the address, e.g. `https://cumulus-listen.<you>.workers.dev`; Cumulus
uses it as `wss://cumulus-listen.<you>.workers.dev`.

## Test locally

```bash
npm run dev
```

then, in another terminal:

```bash
npm test
```

A debug build of Cumulus uses a local relay with
`CUMULUS_DEV_LISTEN_SERVER=ws://127.0.0.1:8787`.
